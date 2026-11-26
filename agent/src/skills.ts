import { createHash } from "node:crypto";
import { existsSync, lstatSync, readFileSync, realpathSync } from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import {
  type JsonObject,
} from "./protocol.js";
import {
  loadSkillsFromDir,
  type ResourceDiagnostic,
  type Skill,
} from "@earendil-works/pi-coding-agent";

const MAX_SKILL_READ_BYTES = 1_048_576;

export interface LoadedLaneSkills {
  skills: Skill[];
  diagnostics: ResourceDiagnostic[];
  signature: string;
  defaultSkillsDir?: string;
  workspaceSkillsDir?: string;
}

export interface LoadLaneSkillsOptions {
  defaultSkillsDir?: string;
  workspaceSkillsDir?: string;
}

function isWithin(parentDir: string, targetPath: string): boolean {
  const rel = relative(parentDir, targetPath);
  return rel !== "" && !rel.startsWith("..") && !isAbsolute(rel);
}

function skillSignature(skills: Skill[]): string {
  return skills
    .map((skill) => `${skill.name}\0${skill.description}\0${skill.filePath}\0${skill.disableModelInvocation}`)
    .join("\n");
}

/**
 * Infer `<book>/workspace/skills` when `session.dir` has the Rust controller's
 * `<book>/model/agent/pi/<lane>` layout.
 */
export function inferWorkspaceSkillsDir(sessionDir: string): string | undefined {
  const piDir = dirname(sessionDir);
  const agentDir = dirname(piDir);
  const modelDir = dirname(agentDir);
  if (basename(piDir) === "pi" && basename(agentDir) === "agent" && basename(modelDir) === "model") {
    return join(dirname(modelDir), "workspace", "skills");
  }
  return undefined;
}

/**
 * Load and validate skills from the committed default directory and the
 * research workspace's `skills/` directory. Broken skills produce diagnostics
 * and are never fatal.
 */
export function loadLaneSkills(options: LoadLaneSkillsOptions): LoadedLaneSkills {
  const skills: Skill[] = [];
  const diagnostics: ResourceDiagnostic[] = [];
  const byName = new Map<string, Skill>();
  const seenRealPaths = new Set<string>();

  const loadDir = (dir: string | undefined, source: string, warnIfMissing: boolean) => {
    if (!dir) return;
    const resolvedDir = resolve(dir);
    if (!existsSync(resolvedDir)) {
      if (warnIfMissing) {
        diagnostics.push({ type: "warning", message: "skill directory does not exist", path: resolvedDir });
      }
      return;
    }
    try {
      const result = loadSkillsFromDir({ dir: resolvedDir, source });
      diagnostics.push(...result.diagnostics);
      for (const skill of result.skills) {
        let realPath: string;
        try {
          realPath = realpathSync(skill.filePath);
        } catch {
          realPath = skill.filePath;
        }
        if (seenRealPaths.has(realPath)) continue;
        const existing = byName.get(skill.name);
        if (existing) {
          diagnostics.push({
            type: "collision",
            message: `name "${skill.name}" collision`,
            path: skill.filePath,
            collision: {
              resourceType: "skill",
              name: skill.name,
              winnerPath: existing.filePath,
              loserPath: skill.filePath,
            },
          });
          continue;
        }
        byName.set(skill.name, skill);
        seenRealPaths.add(realPath);
        skills.push(skill);
      }
    } catch (error) {
      diagnostics.push({
        type: "warning",
        message: error instanceof Error ? error.message : String(error),
        path: resolvedDir,
      });
    }
  };

  loadDir(options.defaultSkillsDir, "default", true);
  loadDir(options.workspaceSkillsDir, "workspace", false);

  return {
    skills,
    diagnostics,
    signature: skillSignature(skills),
    ...(options.defaultSkillsDir ? { defaultSkillsDir: resolve(options.defaultSkillsDir) } : {}),
    ...(options.workspaceSkillsDir ? { workspaceSkillsDir: resolve(options.workspaceSkillsDir) } : {}),
  };
}

export type SkillReadResolution =
  | { kind: "local"; output: JsonObject }
  | { kind: "rewrite"; path: string };

/**
 * Pi's `<available_skills>` tells the model to call `read` with the skill's
 * absolute `<location>`. Rust's `workspace_read` only accepts workspace-relative
 * paths, so default repo skills are served directly from `agent/skills/` and
 * workspace skill absolute paths are rewritten to `skills/...` for Rust.
 */
export function resolveSkillRead(rawPath: string, loaded: LoadedLaneSkills): SkillReadResolution | undefined {
  if (!isAbsolute(rawPath)) return undefined;
  const resolved = resolve(rawPath);
  for (const skill of loaded.skills) {
    const exactFile = resolved === resolve(skill.filePath);
    const inDeclaredDir = basename(skill.filePath) === "SKILL.md" && isWithin(resolve(skill.baseDir), resolved);
    if (!exactFile && !inDeclaredDir) continue;

    if (skill.sourceInfo.source === "workspace" && loaded.workspaceSkillsDir) {
      const rel = relative(loaded.workspaceSkillsDir, resolved);
      if (rel !== "" && !rel.startsWith("..") && !isAbsolute(rel)) {
        return { kind: "rewrite", path: `skills/${rel.split(sep).join("/")}` };
      }
      return undefined;
    }

    if (skill.sourceInfo.source === "default") {
      try {
        const stat = lstatSync(resolved);
        if (stat.isSymbolicLink() || !stat.isFile()) return undefined;
        if (stat.size > MAX_SKILL_READ_BYTES) return undefined;
        const realBase = realpathSync(skill.baseDir);
        const realTarget = realpathSync(resolved);
        if (realTarget !== realpathSync(skill.filePath) && !isWithin(realBase, realTarget)) return undefined;
        const bytes = readFileSync(realTarget);
        const text = bytes.toString("utf8");
        const sha256 = createHash("sha256").update(bytes).digest("hex");
        return {
          kind: "local",
          output: {
            path: rawPath,
            bytes: bytes.byteLength,
            text,
            sha256,
            execution_authorized: false,
            evidence_only: true,
          },
        };
      } catch {
        return undefined;
      }
    }
  }
  return undefined;
}
