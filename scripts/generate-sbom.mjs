#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { basename, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = fileURLToPath(new URL(".", import.meta.url));
const repoRoot = resolve(scriptDir, "..");

const args = process.argv.slice(2);
let output;
let version;
const artifacts = [];

for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--output") {
    output = args[++index];
  } else if (arg === "--version") {
    version = args[++index];
  } else if (arg === "--artifact") {
    artifacts.push(args[++index]);
  } else {
    throw new Error(`unknown argument: ${arg}`);
  }
}

if (!output || !version) {
  throw new Error(
    "usage: generate-sbom.mjs --output <path> --version <semver> [--artifact <path>]...",
  );
}

const cargoMetadata = JSON.parse(
  execFileSync(
    "cargo",
    ["metadata", "--format-version=1", "--locked", "--manifest-path", "Cargo.toml"],
    { cwd: repoRoot, encoding: "utf8" },
  ),
);
const npmLock = JSON.parse(
  readFileSync(resolve(repoRoot, "frontend/package-lock.json"), "utf8"),
);

const cargoRefById = new Map();
const components = [];

for (const pkg of cargoMetadata.packages) {
  const ref = `pkg:cargo/${encodeURIComponent(pkg.name)}@${encodeURIComponent(pkg.version)}`;
  cargoRefById.set(pkg.id, ref);
  components.push({
    type: "library",
    "bom-ref": ref,
    group: "",
    name: pkg.name,
    version: pkg.version,
    purl: ref,
    licenses: pkg.license ? [{ expression: pkg.license }] : undefined,
  });
}

function npmPurl(name, packageVersion) {
  const encodedName = name.startsWith("@")
    ? `${encodeURIComponent(name.slice(0, name.indexOf("/")))}/${encodeURIComponent(
        name.slice(name.indexOf("/") + 1),
      )}`
    : encodeURIComponent(name);
  return `pkg:npm/${encodedName}@${encodeURIComponent(packageVersion)}`;
}

for (const [lockPath, pkg] of Object.entries(npmLock.packages ?? {})) {
  if (!lockPath || !pkg.version) {
    continue;
  }
  const marker = "node_modules/";
  const markerIndex = lockPath.lastIndexOf(marker);
  const inferredName =
    markerIndex === -1 ? lockPath : lockPath.slice(markerIndex + marker.length);
  const name = pkg.name ?? inferredName;
  const ref = npmPurl(name, pkg.version);
  components.push({
    type: "library",
    "bom-ref": ref,
    group: name.startsWith("@") ? name.slice(1, name.indexOf("/")) : "",
    name,
    version: pkg.version,
    purl: ref,
    hashes: pkg.integrity?.startsWith("sha512-")
      ? [
          {
            alg: "SHA-512",
            content: Buffer.from(pkg.integrity.slice("sha512-".length), "base64").toString(
              "hex",
            ),
          },
        ]
      : undefined,
  });
}

for (const artifactPath of artifacts) {
  const bytes = readFileSync(resolve(repoRoot, artifactPath));
  const digest = createHash("sha256").update(bytes).digest("hex");
  const name = basename(artifactPath);
  components.push({
    type: "file",
    "bom-ref": `artifact:${name}:${digest}`,
    name,
    version,
    hashes: [{ alg: "SHA-256", content: digest }],
  });
}

const dependencies = (cargoMetadata.resolve?.nodes ?? []).map((node) => ({
  ref: cargoRefById.get(node.id) ?? node.id,
  dependsOn: node.dependencies
    .map((dependency) => cargoRefById.get(dependency))
    .filter(Boolean),
}));

const sbom = {
  bomFormat: "CycloneDX",
  specVersion: "1.5",
  serialNumber: `urn:uuid:${randomUUID()}`,
  version: 1,
  metadata: {
    timestamp: new Date().toISOString(),
    tools: {
      components: [
        {
          type: "application",
          name: "review-queue-sbom-generator",
          version: "1",
        },
      ],
    },
    component: {
      type: "application",
      "bom-ref": `pkg:generic/review-queue@${encodeURIComponent(version)}`,
      name: "Review Queue",
      version,
    },
  },
  components: components
    .filter(
      (component, index, all) =>
        all.findIndex((candidate) => candidate["bom-ref"] === component["bom-ref"]) ===
        index,
    )
    .map((component) =>
      Object.fromEntries(
        Object.entries(component).filter(([, value]) => value !== undefined),
      ),
    ),
  dependencies,
};

writeFileSync(resolve(repoRoot, output), `${JSON.stringify(sbom, null, 2)}\n`, {
  mode: 0o644,
});
