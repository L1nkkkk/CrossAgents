#!/bin/bash
# 把 Apple 芯片 Mac 上的 CrossAgents 更新到 CrossAgents-releases 的最新版。
#
# Mac 包是 ad-hoc 签名，Squirrel.Mac 不肯装，GUI 自己更新不了；这个脚本做的就是手动那几步：
# 查最新版 → 下载 arm64 zip → 核对 → 退出旧版 → 替换 /Applications 里的 → 去掉隔离标记 → 原来开着就重新打开。
#
#   ./scripts/mac-update.sh                 现在就更新（开着的 CrossAgents 会被退出再打开）
#   ./scripts/mac-update.sh --check         只看有没有新版本，不动
#   ./scripts/mac-update.sh --if-idle       CrossAgents 开着就跳过（给定时任务用）
#   ./scripts/mac-update.sh --install-agent 装一个 LaunchAgent：登录时和每天 9:00 各跑一次 --if-idle
#   ./scripts/mac-update.sh --uninstall-agent
#
# 也可以不 clone 仓库直接跑：
#   curl -fsSL https://raw.githubusercontent.com/L1nkkkk/CrossAgents/main/scripts/mac-update.sh | bash
#
# 要走代理时先设 HTTPS_PROXY（装 LaunchAgent 时会把当前的 HTTPS_PROXY / HTTP_PROXY / ALL_PROXY 记进去）。

set -euo pipefail

REPO="L1nkkkk/CrossAgents-releases"
APP="/Applications/CrossAgents.app"
LABEL="dev.crossagents.updater"
AGENT_DIR="$HOME/Library/Application Support/CrossAgents-updater"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/Library/Logs/CrossAgents-updater.log"

say() { printf '[%s] %s\n' "$(date '+%F %T')" "$*"; }
die() { say "错误：$*" >&2; exit 1; }

usage() {
  if [ -f "$0" ]; then sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; else echo "用法见 https://github.com/L1nkkkk/CrossAgents/blob/main/scripts/mac-update.sh"; fi
  exit 0
}

mode=now
case "${1:-}" in
  "") ;;
  --check) mode=check ;;
  --if-idle) mode=idle ;;
  --install-agent) mode=install ;;
  --uninstall-agent) mode=uninstall ;;
  -h|--help) usage ;;
  *) die "不认识的参数 $1（--help 看用法）" ;;
esac

[ "$(uname -s)" = Darwin ] || die "只能在 macOS 上跑"

# ------------------------------------------------------------------ LaunchAgent
if [ "$mode" = uninstall ]; then
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"
  rm -rf "$AGENT_DIR"
  say "已卸载自动更新"
  exit 0
fi

if [ "$mode" = install ]; then
  mkdir -p "$AGENT_DIR" "$(dirname "$PLIST")" "$(dirname "$LOG")"
  script="$AGENT_DIR/mac-update.sh"
  # 从 curl | bash 跑的时候 $0 不是文件，就从仓库取一份。
  if [ -f "$0" ]; then cp "$0" "$script"; else
    curl -fsSL "https://raw.githubusercontent.com/L1nkkkk/CrossAgents/main/scripts/mac-update.sh" -o "$script"
  fi
  chmod +x "$script"
  env_xml=""
  for k in HTTPS_PROXY HTTP_PROXY ALL_PROXY NO_PROXY; do
    v="${!k:-}"
    [ -n "$v" ] && env_xml="$env_xml<key>$k</key><string>$v</string>"
  done
  cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array><string>/bin/bash</string><string>$script</string><string>--if-idle</string></array>
  <key>RunAtLoad</key><true/>
  <key>StartCalendarInterval</key><dict><key>Hour</key><integer>9</integer><key>Minute</key><integer>0</integer></dict>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>/usr/bin:/bin:/usr/sbin:/sbin</string>$env_xml</dict>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict>
</plist>
EOF
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$PLIST"
  say "已装好：登录时和每天 9:00 检查一次，CrossAgents 开着就跳过到下次。日志：$LOG"
  exit 0
fi

# ------------------------------------------------------------------ 查版本
[ "$(uname -m)" = arm64 ] || die "这台是 Intel Mac，发布页没有 x64 的包；按 docs/INSTALL-MAC.md 第 2 节自己打包"

# a 比 b 新就返回 0。只比数字段（X.Y.Z）。
newer() {
  local IFS=.
  local -a a=($1) b=($2)
  for i in 0 1 2; do
    local x=${a[i]:-0} y=${b[i]:-0}
    ((10#$x > 10#$y)) && return 0
    ((10#$x < 10#$y)) && return 1
  done
  return 1
}

installed=""
[ -d "$APP" ] && installed=$(defaults read "$APP/Contents/Info" CFBundleShortVersionString 2>/dev/null || true)

# releases/latest 会跳到 .../releases/tag/vX.Y.Z，不走 API，不受匿名限流影响。
latest_url=$(curl -fsSL --retry 3 --connect-timeout 15 -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") \
  || die "连不上 GitHub（国内网络要先设 HTTPS_PROXY）"
latest=${latest_url##*/v}
[[ "$latest" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "认不出最新版本号：$latest_url"

say "已装：${installed:-没有}  最新：$latest"
if [ -n "$installed" ] && ! newer "$latest" "$installed"; then
  say "已是最新"
  exit 0
fi
[ "$mode" = check ] && { say "有新版本，跑 $0 更新"; exit 0; }

running=no
pgrep -f "$APP/Contents/MacOS/" >/dev/null 2>&1 && running=yes
if [ "$mode" = idle ] && [ "$running" = yes ]; then
  say "CrossAgents 开着，这次不更新"
  exit 0
fi

# ------------------------------------------------------------------ 下载与核对
lock="${TMPDIR:-/tmp}/crossagents-updater.lock"
mkdir "$lock" 2>/dev/null || die "另一个更新在跑（不是的话删掉 $lock）"
work=$(mktemp -d)
trap 'rm -rf "$work" "$lock"' EXIT

zip="CrossAgents-$latest-arm64.zip"
say "下载 $zip"
code=$(curl -sSL --retry 3 --connect-timeout 15 -o "$work/$zip" -w '%{http_code}' \
  "https://github.com/$REPO/releases/download/v$latest/$zip") || die "下载失败"
if [ "$code" = 404 ]; then
  # 发版时 Windows 包先上，Mac 包由 GitHub Actions 晚几分钟补上。
  say "v$latest 还没有 Mac 包（CI 可能还在打），下次再试"
  exit 0
fi
[ "$code" = 200 ] || die "下载返回 HTTP $code"

ditto -x -k "$work/$zip" "$work/new"
new="$work/new/CrossAgents.app"
[ -d "$new" ] || die "zip 里没有 CrossAgents.app"
got=$(defaults read "$new/Contents/Info" CFBundleShortVersionString)
[ "$got" = "$latest" ] || die "zip 里的版本是 $got，不是 $latest"
codesign --verify --deep --strict "$new" || die "签名校验没过，不装"
[ -x "$new/Contents/Resources/bin/crossagents-link" ] || die "包里缺 crossagents-link，不装"

# ------------------------------------------------------------------ 替换
if [ "$running" = yes ]; then
  say "退出正在运行的 CrossAgents"
  osascript -e 'tell application "CrossAgents" to quit' >/dev/null 2>&1 || true
  # 远程模式下退出前要关隧道，最多 5 秒。
  for _ in $(seq 1 20); do
    pgrep -f "$APP/Contents/MacOS/" >/dev/null 2>&1 || break
    sleep 1
  done
  pgrep -f "$APP/Contents/MacOS/" >/dev/null 2>&1 && pkill -f "$APP/Contents/MacOS/" || true
  sleep 1
fi

backup=""
if [ -d "$APP" ]; then
  backup="$work/old.app"
  mv "$APP" "$backup" || die "移不走旧版（/Applications 没有写权限？）"
fi
if ! ditto "$new" "$APP"; then
  rm -rf "$APP"
  [ -n "$backup" ] && mv "$backup" "$APP"
  die "复制新版失败，已恢复旧版"
fi
xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true

say "已更新到 $latest"
if [ "$running" = yes ]; then
  open "$APP"
  say "已重新打开"
fi
