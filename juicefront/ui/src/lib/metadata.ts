import { execSync } from "child_process";

export interface RepoMetadata {
  owner: string;
  name: string;
  defaultBranch: string;
  url: string;
  latestCommitRef: string;
  latestCommitHash: string;
  latestCommitShortHash: string;
}

function exec(cmd: string): string {
  try {
    return execSync(cmd, { cwd: process.cwd() }).toString().trim();
  } catch {
    return "unknown";
  }
}

const gitRef = exec("git rev-parse --abbrev-ref HEAD 2>/dev/null");
const gitHash = exec("git rev-parse HEAD 2>/dev/null");
const gitShortHash = gitHash !== "unknown" ? gitHash.slice(0, 7) : "unknown";

export const REPO_METADATA: RepoMetadata = {
  owner: "create-juicey-app",
  name: "juicebox2",
  defaultBranch: "main",
  url: "https://github.com/create-juicey-app/juicebox2",
  latestCommitRef: gitRef,
  latestCommitHash: gitHash,
  latestCommitShortHash: gitShortHash,
};

export const getRepoUrl = (): string => REPO_METADATA.url;

export const getRepoPathUrl = (
  path: string,
  ref: string = REPO_METADATA.defaultBranch,
): string =>
  `${REPO_METADATA.url}/blob/${encodeURIComponent(ref)}/${path.replace(/^\/+/, "")}`;

export const getFormattedCommitLabel = (includeBranch = true): string =>
  includeBranch
    ? REPO_METADATA.latestCommitRef
    : REPO_METADATA.latestCommitShortHash;

export const getProjectStructuredData = () => ({
  "@context": "https://schema.org",
  "@type": "SoftwareSourceCode",
  name: "Juicebox2",
  version: REPO_METADATA.latestCommitShortHash,
  programmingLanguage: "TypeScript",
  codeRepository: REPO_METADATA.url,
  license: "https://opensource.org/licenses/MIT",
  source: {
    "@type": "URL",
    "@id": REPO_METADATA.url,
  },
});
