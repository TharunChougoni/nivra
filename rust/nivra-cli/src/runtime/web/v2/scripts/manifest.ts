import { createHash } from "node:crypto";
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const hash = (bytes: Uint8Array) =>
  createHash("sha256").update(bytes).digest("hex");
const list = (directory: string): string[] =>
  readdirSync(join(root, directory), { withFileTypes: true }).flatMap((v) =>
    v.isDirectory() ? list(join(directory, v.name)) : [join(directory, v.name)],
  );
const sources = [
  "package.json",
  "package-lock.json",
  "vite.config.ts",
  "tsconfig.json",
  "index.html",
  ...list("src"),
  ...list("scripts"),
].sort();
const fingerprints = Object.fromEntries(
  sources.map((name) => [name, hash(readFileSync(join(root, name)))]),
);
const path = join(root, "bundle/manifest.json");
if (process.argv.includes("--check")) {
  const previous = JSON.parse(readFileSync(path, "utf8")) as {
    sources: Record<string, string>;
    assets: Record<string, string>;
  };
  if (JSON.stringify(previous.sources) !== JSON.stringify(fingerprints))
    throw Error("Frontend bundle is stale. Run npm run build in web/v2.");
  for (const [name, expected] of Object.entries(previous.assets))
    if (hash(readFileSync(join(root, "bundle", name))) !== expected)
      throw Error("Asset mismatch: " + name);
  console.log("Frontend sources and committed bundle match.");
} else {
  const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8")) as {
    dependencies: Record<string, string>;
  };
  let licenses = "Nivra client workspace — third-party licenses\n\n";
  for (const [name, version] of Object.entries(pkg.dependencies)) {
    const base = join(root, "node_modules", name);
    const license = readdirSync(base).find((v) =>
      /^licen[sc]e(\.txt|\.md)?$/i.test(v),
    );
    licenses += `\n${name} ${version}\n${"=".repeat(48)}\n${license ? readFileSync(join(base, license), "utf8") : "See package license and npm metadata."}\n`;
  }
  writeFileSync(join(root, "bundle/LICENSES.txt"), licenses);
  const assets = Object.fromEntries(
    readdirSync(join(root, "bundle"))
      .filter((n) => !["manifest.json", "SOURCES.md"].includes(n))
      .sort()
      .map((n) => [n, hash(readFileSync(join(root, "bundle", n)))]),
  );
  writeFileSync(
    path,
    JSON.stringify({ sources: fingerprints, assets }, null, 2) + "\n",
  );
  writeFileSync(
    join(root, "bundle/SOURCES.md"),
    "# Bundled client assets\n\nBuilt locally with pinned package-lock.json. No runtime CDN or Node service.\n\n| package | version | file | source | sha-256 |\n|---|---|---|---|---|\n" +
      Object.entries(assets)
        .map(
          ([name, sha]) =>
            `| workspace | source | ${name} | local React source | ${sha} |`,
        )
        .join("\n") +
      "\n\n" +
      Object.entries(pkg.dependencies)
        .map(([name, version]) => `${name}: ${version}`)
        .join("\n") +
      "\n",
  );
}
