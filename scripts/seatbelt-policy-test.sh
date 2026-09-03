#!/bin/bash
# Seatbelt 沙箱策略直测：不依赖 agent / 模型，直接用 /usr/bin/sandbox-exec
# 验证 src/sandbox.rs::sandbox_policy 的隔离效果。
#
# 用法（必须在普通终端运行——Terminal.app / iTerm；在 Codex 等已嵌套
# Seatbelt 的环境里 sandbox-exec 无法应用策略，脚本会直接报错退出）：
#   ./scripts/seatbelt-policy-test.sh
#
# 期望：8 项 ✅、0 项 ❌（opt-in 网络探测在代理/断网环境可能显示 ⚠️，
# 不影响写隔离结论）。
set -u

SANDBOX_EXEC=/usr/bin/sandbox-exec

if [ "$(uname -s)" != "Darwin" ]; then
  echo "❌ 此脚本仅适用于 macOS（Seatbelt / sandbox-exec）"
  exit 1
fi

# 探测 sandbox-exec 是否真能应用策略（嵌套沙箱环境会报
# sandbox_apply: Operation not permitted，此时下面的断言会"假通过"，
# 必须提前拦下）。
if ! "$SANDBOX_EXEC" -p '(version 1)(allow default)' /usr/bin/true 2>/dev/null; then
  echo "❌ sandbox-exec 无法在本环境应用策略（sandbox_apply: Operation not permitted）。"
  echo "   你可能在 Codex / 其他已嵌套 Seatbelt 的环境里。请在普通终端"
  echo "   （Terminal.app / iTerm）中运行本脚本。"
  exit 1
fi

BASE=$(mktemp -d /tmp/sandbox-test-XXXXXX)
trap 'rm -rf "$BASE"' EXIT
ROOT="$BASE/root"; OUT="$BASE/out"; TMP="$BASE/tmp"
mkdir -p "$ROOT" "$OUT" "$TMP"
CROOT=$(cd "$ROOT" && pwd -P); COUT=$(cd "$OUT" && pwd -P); CTMP=$(cd "$TMP" && pwd -P)

# 与 src/sandbox.rs::sandbox_policy 完全一致的策略（network off）
POLICY="(version 1)(allow default)(deny network*)(deny file-write*)(allow file-write* (subpath \"$CROOT\"))(allow file-write* (subpath \"$CTMP\"))"
sbx() { "$SANDBOX_EXEC" -p "$POLICY" "$@"; }
PASS=0; FAIL=0
ok()   { echo "  ✅ $1"; PASS=$((PASS+1)); }
bad()  { echo "  ❌ $1"; FAIL=$((FAIL+1)); }

echo "== 1. 越界写必须被拒 =="
sbx /bin/sh -c "echo x > $COUT/abs.txt" 2>/dev/null
[ -f "$COUT/abs.txt" ] && bad "绝对路径直写泄漏了" || ok "绝对路径直写被拒"

echo secret > "$COUT/victim.txt"
sbx /bin/sh -c "ln -s $COUT/victim.txt $CROOT/link && echo pwned > $CROOT/link" 2>/dev/null
[ "$(cat "$COUT/victim.txt")" = "secret" ] && ok "symlink 逃逸写被拒" || bad "symlink 逃逸泄漏了"

echo x > "$ROOT/x.txt"
sbx /bin/sh -c "mv $CROOT/x.txt $COUT/x.txt" 2>/dev/null
{ [ -f "$ROOT/x.txt" ] && [ ! -f "$OUT/x.txt" ]; } && ok "rename 跨界被拒" || bad "rename 跨界泄漏了"

echo keep > "$OUT/victim-u.txt"
sbx /bin/sh -c "rm -f $COUT/victim-u.txt" 2>/dev/null
[ -f "$OUT/victim-u.txt" ] && ok "unlink 外部文件被拒" || bad "unlink 外部文件成功了"

echo keep > "$OUT/victim-h.txt"
sbx /bin/sh -c "ln $COUT/victim-h.txt $CROOT/hl && echo pwned > $CROOT/hl" 2>/dev/null
[ "$(cat "$COUT/victim-h.txt")" = "keep" ] && ok "hardlink 映射写被拒" || bad "hardlink 泄漏了"

echo "== 2. ROOT / TMPDIR 内正常写入必须放行（策略不能过严） =="
sbx /bin/sh -c "echo ok > $CROOT/in.txt" && [ -f "$ROOT/in.txt" ] && ok "ROOT 内写入放行" || bad "ROOT 内写入被误拒"
sbx /bin/sh -c "echo ok > $CTMP/t.txt" && [ -f "$TMP/t.txt" ] && ok "TMPDIR 内写入放行" || bad "TMPDIR 写入被误拒"

echo "== 3. 网络默认关 =="
sbx /bin/sh -c "bash -c 'echo > /dev/tcp/1.1.1.1/80'" 2>/dev/null && bad "默认策略下网络竟然通" || ok "默认网络被拒"

echo "== 4. KARAKURI_SANDBOX_NETWORK=1 等价策略：网络放行 =="
POLICY_ON="(version 1)(allow default)(deny file-write*)(allow file-write* (subpath \"$CROOT\"))(allow file-write* (subpath \"$CTMP\"))"
"$SANDBOX_EXEC" -p "$POLICY_ON" /bin/sh -c "bash -c 'echo > /dev/tcp/1.1.1.1/80'" 2>/dev/null \
  && ok "opt-in 后网络放行" || echo "  ⚠️  opt-in 网络探测失败（检查代理/网络环境，不影响写隔离结论）"

echo "----------------------------------------"
echo "结果：$PASS 通过, $FAIL 失败（失败数应为 0）"
[ "$FAIL" -eq 0 ]
