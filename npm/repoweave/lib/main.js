"use strict";

const os = require("os");
const path = require("path");

// Map Node.js platform/arch to the npm package that contains the binary.
const PLATFORMS = {
  "darwin arm64": "@repoweave/darwin-arm64",
  "darwin x64": "@repoweave/darwin-x64",
  "linux arm64": "@repoweave/linux-arm64",
  "linux x64": "@repoweave/linux-x64",
  "win32 x64": "@repoweave/windows-x64",
};

function getBinaryPath() {
  const key = `${os.platform()} ${os.arch()}`;
  const pkg = PLATFORMS[key];
  if (!pkg) {
    throw new Error(
      `Unsupported platform: ${os.platform()} ${os.arch()}. ` +
        `repoweave currently supports: ${Object.keys(PLATFORMS).join(", ")}`
    );
  }

  // The platform package exports the path to its binary via its package.json "main" field.
  // We resolve the package directory and construct the binary path.
  const pkgDir = path.dirname(require.resolve(`${pkg}/package.json`));
  const binName = os.platform() === "win32" ? "rwv.exe" : "rwv";
  return path.join(pkgDir, "bin", binName);
}

module.exports = { getBinaryPath };
