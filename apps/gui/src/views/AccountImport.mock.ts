// Mock data for importing a login already on this machine (preview:mock). The two files and
// the warnings are the hub's (crates/hub/src/account_import.rs); keep the words equal, the
// GUI only shows them.

import type { ImportableAccount } from "@bindings/ImportableAccount";

export function importCandidates(): ImportableAccount[] {
  return [
    {
      provider: "codex",
      source: "C:/Users/me/.codex/auth.json",
      warning: "导入的是同一份 ChatGPT 登录令牌。之后这个账号刷新令牌时，原来的 Codex 会被登出，需要重新登录。",
    },
    {
      provider: "claude",
      source: "C:/Users/me/.claude/.credentials.json",
      warning: "导入的是同一份 Claude 登录凭据。之后两个目录都可能刷新令牌，刷新后另一边可能需要重新登录。",
    },
  ];
}
