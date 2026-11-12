// Open in BigText: hands files and folders from VS Code to BigText, and offers it for files too
// big to edit comfortably in VS Code.
"use strict";

const vscode = require("vscode");
const cp = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { findBigText, target, humanSize } = require("./find");

const DOWNLOAD = "https://erkinbek.github.io/bigtext/#download";

/** BigText's executable: the bigtext.path setting, or where the installers put it. */
function exePath() {
  const set = vscode.workspace.getConfiguration("bigtext").get("path", "").trim();
  if (set) return fs.existsSync(set) ? set : null;
  return findBigText(process.platform, process.env, os.homedir(), fs.existsSync);
}

async function launch(args) {
  const exe = exePath();
  if (!exe) {
    const download = vscode.l10n.t("Download");
    const setPath = vscode.l10n.t("Set path");
    const pick = await vscode.window.showErrorMessage(
      vscode.l10n.t("BigText was not found. Install it, or set its path in the bigtext.path setting."),
      download,
      setPath,
    );
    if (pick === download) vscode.env.openExternal(vscode.Uri.parse(DOWNLOAD));
    if (pick === setPath) vscode.commands.executeCommand("workbench.action.openSettings", "bigtext.path");
    return;
  }
  const failed = (e) => vscode.window.showErrorMessage(vscode.l10n.t("Could not start BigText: {0}", e.message));
  try {
    // A running BigText takes the files itself, so this returns at once.
    const child = cp.spawn(exe, args, { detached: true, stdio: "ignore" });
    child.on("error", failed);
    child.unref();
  } catch (e) {
    failed(e);
  }
}

/** The file of the active tab, also when VS Code would not show it because it is too large. */
function activeTabUri() {
  const input = vscode.window.tabGroups.activeTabGroup.activeTab?.input;
  return input && input.uri instanceof vscode.Uri ? input.uri : undefined;
}

/** Files already offered to BigText in this session. */
const offered = new Set();

/** Offers BigText for a file that is bigger than the bigtext.suggestAboveMB setting. */
async function offer(tab) {
  const config = vscode.workspace.getConfiguration("bigtext");
  const mb = config.get("suggestAboveMB", 50);
  const uri = tab.input && tab.input.uri;
  if (!mb || !(uri instanceof vscode.Uri) || uri.scheme !== "file" || offered.has(uri.fsPath)) return;
  let size;
  try {
    size = (await fs.promises.stat(uri.fsPath)).size;
  } catch {
    return;
  }
  if (size < mb * 2 ** 20) return;
  offered.add(uri.fsPath);
  const open = vscode.l10n.t("Open in BigText");
  const never = vscode.l10n.t("Don't ask again");
  const pick = await vscode.window.showInformationMessage(
    vscode.l10n.t("{0} is {1}. BigText opens it at once and never loads it into memory.", path.basename(uri.fsPath), humanSize(size)),
    open,
    never,
  );
  if (pick === open) launch([uri.fsPath]);
  if (pick === never) config.update("suggestAboveMB", 0, vscode.ConfigurationTarget.Global);
}

function activate(context) {
  context.subscriptions.push(
    vscode.commands.registerCommand("bigtext.open", (uri) => {
      // From the editor (or its tab), at the caret's line; from the explorer, the file.
      const editor = vscode.window.activeTextEditor;
      let line;
      if (editor && (!uri || editor.document.uri.toString() === uri.toString())) {
        uri = editor.document.uri;
        line = editor.selection.active.line + 1;
      }
      uri = uri || activeTabUri();
      if (!(uri instanceof vscode.Uri) || uri.scheme !== "file") {
        vscode.window.showInformationMessage(vscode.l10n.t("Open a file that is on disk first."));
        return;
      }
      launch([target(uri.fsPath, line)]);
    }),
    vscode.commands.registerCommand("bigtext.openFolder", (uri) => {
      uri = uri instanceof vscode.Uri ? uri : vscode.workspace.workspaceFolders?.[0]?.uri;
      if (uri && uri.scheme === "file") launch([uri.fsPath]);
    }),
    vscode.window.tabGroups.onDidChangeTabs((e) => {
      for (const tab of e.opened) offer(tab);
    }),
  );
}

function deactivate() {}

module.exports = { activate, deactivate };
