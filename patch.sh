#!/usr/bin/env bash
#
# patch.sh - 修补 ~/.cargo 缓存中 xpty-0.3.6 的 musl 编译错误
#
# 问题：xpty-0.3.6 的 src/unix.rs 直接调用 libc::close_range()，
#       但 libc crate 仅为 glibc(gnu) 目标定义了该函数封装，
#       musl 目标下不存在，导致编译报错 E0425。
# 修复：改用 libc::syscall(libc::SYS_close_range, ...) 发起系统调用，
#       SYS_close_range 常量在 gnu 与 musl 目标下均有定义。
# 注意：registry 缓存解压后的源码 cargo 不会逐文件校验；
#       若存在 .cargo-checksum.json（如 vendor 目录），则同步更新其 sha256。

set -euo pipefail

CRATE_NAME="xpty"
CRATE_VERSION="0.3.6"
TARGET_FILE="src/unix.rs"

# 原始代码与替换代码（单行精确匹配）
OLD_CODE='let ret = unsafe { libc::close_range(3, libc::c_uint::MAX, 0) };'
NEW_CODE='let ret = unsafe { libc::syscall(libc::SYS_close_range, 3, libc::c_uint::MAX, 0) };'

# 打印带前缀的日志信息
log() {
  printf '[patch] %s\n' "$*"
}

# 打印错误信息并退出
die() {
  printf '[patch][ERROR] %s\n' "$*" >&2
  exit 1
}

# 计算文件的 sha256 哈希值（兼容 macOS 与 Linux）
sha256_of() {
  local file="$1"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    sha256sum "$file" | awk '{print $1}'
  fi
}

# 修补单个 crate 缓存目录：替换源码并更新 .cargo-checksum.json
patch_crate_dir() {
  local crate_dir="$1"
  local src_file="${crate_dir}/${TARGET_FILE}"
  local checksum_file="${crate_dir}/.cargo-checksum.json"

  [[ -f "$src_file" ]] || die "源码文件不存在: $src_file"

  # 已修补则跳过
  if grep -qF "$NEW_CODE" "$src_file"; then
    log "已修补过，跳过: $crate_dir"
    return 0
  fi

  # 确认原始代码存在
  grep -qF "$OLD_CODE" "$src_file" || die "未找到目标代码行，crate 内容与预期不符: $src_file"

  # 备份原始文件（仅首次）
  [[ -f "${src_file}.bak" ]] || cp "$src_file" "${src_file}.bak"

  # 执行替换（用 python 做字面量替换，避免 sed 转义问题）
  OLD_CODE="$OLD_CODE" NEW_CODE="$NEW_CODE" python3 - "$src_file" <<'PYEOF'
import os, sys
# 读取源码文件，替换目标代码行后写回
path = sys.argv[1]
old, new = os.environ["OLD_CODE"], os.environ["NEW_CODE"]
with open(path) as f:
    content = f.read()
assert old in content, "old code not found"
with open(path, "w") as f:
    f.write(content.replace(old, new, 1))
PYEOF
  log "已替换 close_range 调用: $src_file"

  # 若存在校验和文件（vendor 目录场景），同步更新其中该文件的 sha256
  if [[ -f "$checksum_file" ]]; then
    local new_hash
    new_hash="$(sha256_of "$src_file")"
    NEW_HASH="$new_hash" TARGET_FILE="$TARGET_FILE" python3 - "$checksum_file" <<'PYEOF'
import json, os, sys
# 更新 .cargo-checksum.json 中被修改文件的 sha256 记录
path = sys.argv[1]
with open(path) as f:
    data = json.load(f)
data["files"][os.environ["TARGET_FILE"]] = os.environ["NEW_HASH"]
with open(path, "w") as f:
    json.dump(data, f)
PYEOF
    log "已更新校验和: $checksum_file (${TARGET_FILE} -> ${new_hash})"
  fi
}

# 主流程：查找所有 registry 缓存中的 xpty 目录并逐个修补
main() {
  local registry_src="${CARGO_HOME:-$HOME/.cargo}/registry/src"
  [[ -d "$registry_src" ]] || die "registry 目录不存在: $registry_src"

  local found=0
  local dir
  for dir in "$registry_src"/*/"${CRATE_NAME}-${CRATE_VERSION}"; do
    [[ -d "$dir" ]] || continue
    found=1
    log "发现 crate 缓存: $dir"
    patch_crate_dir "$dir"
  done

  [[ "$found" -eq 1 ]] || die "未找到 ${CRATE_NAME}-${CRATE_VERSION}，请先执行 cargo fetch"
  log "全部完成"
}

main "$@"
