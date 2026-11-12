"use strict";

const test = require("node:test");
const assert = require("node:assert");
const { candidates, findBigText, target, humanSize } = require("../find");

test("Windows: the installer first, then Scoop, Cargo and PATH", () => {
  const env = { LOCALAPPDATA: "C:\\Users\\a\\AppData\\Local", ProgramFiles: "C:\\Program Files", PATH: "C:\\tools;C:\\Program Files\\BigText" };
  const list = candidates("win32", env, "C:\\Users\\a");
  assert.deepStrictEqual(list, [
    "C:\\Users\\a\\AppData\\Local\\Programs\\BigText\\bigtext.exe",
    "C:\\Program Files\\BigText\\bigtext.exe",
    "C:\\Users\\a\\scoop\\apps\\bigtext\\current\\bigtext.exe",
    "C:\\Users\\a\\.cargo\\bin\\bigtext.exe",
    "C:\\tools\\bigtext.exe",
  ]);
});

test("macOS: the app, Homebrew, Cargo and PATH", () => {
  const list = candidates("darwin", { PATH: "/usr/bin:/opt/homebrew/bin" }, "/Users/a");
  assert.strictEqual(list[0], "/Applications/BigText.app/Contents/MacOS/bigtext");
  assert.ok(list.includes("/opt/homebrew/bin/bigtext"));
  assert.ok(list.includes("/Users/a/.cargo/bin/bigtext"));
  assert.ok(list.includes("/usr/bin/bigtext"));
  assert.strictEqual(new Set(list).size, list.length);
});

test("Linux: the .deb and AUR place first", () => {
  const list = candidates("linux", { PATH: "/usr/bin:/home/a/bin" }, "/home/a");
  assert.strictEqual(list[0], "/usr/bin/bigtext");
  assert.ok(list.includes("/home/a/.local/bin/bigtext"));
  assert.ok(list.includes("/home/a/bin/bigtext"));
});

test("finds the first that exists", () => {
  const env = { PATH: "/a:/b" };
  assert.strictEqual(findBigText("linux", env, "/home/x", (p) => p === "/b/bigtext"), "/b/bigtext");
  assert.strictEqual(findBigText("linux", env, "/home/x", () => false), null);
});

test("FILE:LINE and sizes", () => {
  assert.strictEqual(target("C:\\logs\\app.log", 42), "C:\\logs\\app.log:42");
  assert.strictEqual(target("/var/log/syslog"), "/var/log/syslog");
  assert.strictEqual(humanSize(3 * 2 ** 30 + 2 ** 29), "3.5 GB");
  assert.strictEqual(humanSize(320 * 2 ** 20), "320 MB");
});
