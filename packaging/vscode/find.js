// Where BigText is installed, and how to hand it a file. No VS Code API here, so it can be
// tested with `node --test`.
"use strict";

const path = require("path");

/** The places BigText's installers and package managers put it, most likely first. */
function candidates(platform, env, home) {
  const p = platform === "win32" ? path.win32 : path.posix;
  const exe = platform === "win32" ? "bigtext.exe" : "bigtext";
  const list = [];
  if (platform === "win32") {
    // The installer: per user, or for all users.
    if (env.LOCALAPPDATA) list.push(p.join(env.LOCALAPPDATA, "Programs", "BigText", exe));
    for (const pf of [env.ProgramFiles, env.ProgramW6432]) {
      if (pf) list.push(p.join(pf, "BigText", exe));
    }
    if (env.SCOOP) list.push(p.join(env.SCOOP, "apps", "bigtext", "current", exe));
    if (home) list.push(p.join(home, "scoop", "apps", "bigtext", "current", exe));
  } else if (platform === "darwin") {
    list.push("/Applications/BigText.app/Contents/MacOS/bigtext");
    if (home) list.push(p.join(home, "Applications", "BigText.app", "Contents", "MacOS", "bigtext"));
    list.push("/opt/homebrew/bin/bigtext", "/usr/local/bin/bigtext");
  } else {
    list.push("/usr/bin/bigtext", "/usr/local/bin/bigtext");
    if (home) list.push(p.join(home, ".local", "bin", "bigtext"));
    list.push("/home/linuxbrew/.linuxbrew/bin/bigtext");
  }
  if (home) list.push(p.join(home, ".cargo", "bin", exe));
  const pathVar = env.PATH || env.Path || "";
  for (const dir of pathVar.split(platform === "win32" ? ";" : ":")) {
    if (dir) list.push(p.join(dir, exe));
  }
  return [...new Set(list)];
}

/** The first candidate that exists, or null. */
function findBigText(platform, env, home, exists) {
  return candidates(platform, env, home).find((c) => exists(c)) || null;
}

/** The argument that opens `file` (at 1-based `line` when given): BigText reads FILE:LINE. */
function target(file, line) {
  return line ? `${file}:${line}` : file;
}

/** "1.5 GB" or "320 MB". */
function humanSize(bytes) {
  const gb = bytes / 2 ** 30;
  return gb >= 1 ? `${gb.toFixed(1)} GB` : `${Math.round(bytes / 2 ** 20)} MB`;
}

module.exports = { candidates, findBigText, target, humanSize };
