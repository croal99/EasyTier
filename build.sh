#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT_DIR"

TOOLCHAIN="${TOOLCHAIN:-1.95}"
METHOD="release"
BIN_NAME="easytier-core"
TARGET=""
CLEAN_BEFORE_BUILD=0
OFFLINE_BUILD=0
FEATURES=""
USE_UPX="auto"
PROTOC_BIN="${PROTOC:-}"
MENU_MODE=0
EXTRA_CARGO_ARGS=()
BUILD_ENV=()

COLOR_RESET=""
COLOR_BOLD=""
COLOR_RED=""
COLOR_GREEN=""
COLOR_YELLOW=""
COLOR_BLUE=""
COLOR_CYAN=""

# Initialize ANSI colors when the current output supports them.
init_colors() {
  if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
    COLOR_RESET=$'\033[0m'
    COLOR_BOLD=$'\033[1m'
    COLOR_RED=$'\033[31m'
    COLOR_GREEN=$'\033[32m'
    COLOR_YELLOW=$'\033[33m'
    COLOR_BLUE=$'\033[34m'
    COLOR_CYAN=$'\033[36m'
  fi
}

# Print the script help text and examples.
usage() {
  cat <<'EOF'
Usage:
  ./build.sh [options]

Options:
  -m, --method <name>      Build method: debug | release | release-small | official
  -b, --bin <name>         Binary name: easytier-core | easytier-cli
  -t, --target <triple>    Rust target triple, for example x86_64-unknown-linux-musl
      --features <list>    Override cargo features, for example "jemalloc" or "mimalloc"
      --clean              Clean the easytier build artifacts before building
      --offline            Run cargo in offline mode
      --upx                Force UPX compression in official mode
      --no-upx             Disable UPX compression in official mode
      --menu               Force interactive menu mode
      --help               Show this help message

Behavior:
  - Running ./build.sh without arguments enters interactive menu mode
  - Running ./build.sh with arguments keeps the non-interactive CLI mode

Methods:
  debug
      cargo build

  release
      cargo build --release

  release-small
      cargo build --profile release-small

  official
      Emulate the project CI flow:
      - cargo build --release
      - choose allocator feature by target when --features is not provided
      - use UPX compression when available (or when --upx is given)

Examples:
  ./build.sh
  ./build.sh --method release --bin easytier-core
  ./build.sh --method release-small --bin easytier-core --target x86_64-unknown-linux-musl
  ./build.sh --method official --bin easytier-core --target x86_64-unknown-linux-musl --clean
  ./build.sh --method official --bin easytier-cli --target aarch64-unknown-linux-musl --features mimalloc
EOF
}

# Print a formatted log line with level and color.
print_log() {
  local level="$1"
  local color="$2"
  shift 2
  printf '%s[%s]%s %s\n' "${color}" "${level}" "${COLOR_RESET}" "$*"
}

# Print a formatted UI line to stderr so interactive menus remain visible
# even when the function result is captured through command substitution.
ui_print() {
  printf '%s\n' "$*" >&2
}

# Print a standard informational message.
log_info() {
  print_log "INFO" "${COLOR_BLUE}" "$*"
}

# Print a step marker for larger actions.
log_step() {
  print_log "STEP" "${COLOR_CYAN}${COLOR_BOLD}" "$*"
}

# Print a warning message.
log_warn() {
  print_log "WARN" "${COLOR_YELLOW}" "$*"
}

# Print a success message.
log_success() {
  print_log "OK" "${COLOR_GREEN}" "$*"
}

# Print an error message and exit immediately.
die() {
  print_log "ERROR" "${COLOR_RED}${COLOR_BOLD}" "$*" >&2
  exit 1
}

# Return the Rust host target for the configured toolchain.
detect_host_target() {
  rustc "+${TOOLCHAIN}" -vV | sed -n 's/^host: //p'
}

# Detect an available protoc binary from PATH or common local cache paths.
detect_protoc() {
  local candidate=""
  local common_paths=(
    "${PROTOC_BIN}"
    "$(command -v protoc 2>/dev/null || true)"
    "/tmp/protoc-35.1/bin/protoc"
    "/private/tmp/protoc-35.1/bin/protoc"
  )

  for candidate in "${common_paths[@]}"; do
    if [[ -n "$candidate" && -x "$candidate" ]]; then
      printf '%s' "$candidate"
      return 0
    fi
  done

  return 1
}

# Ensure a usable protoc binary is available before starting the real build.
ensure_protoc() {
  if [[ -n "$PROTOC_BIN" && -x "$PROTOC_BIN" ]]; then
    return 0
  fi

  if PROTOC_BIN="$(detect_protoc)"; then
    return 0
  fi

  die "protoc not found; set PROTOC=/path/to/protoc or install protoc 35.1"
}

# Validate that the requested method is supported.
validate_method() {
  case "$1" in
    debug|release|release-small|official) ;;
    *)
      die "unsupported method: $1"
      ;;
  esac
}

# Validate that the requested binary is supported by the easytier package.
validate_bin() {
  case "$1" in
    easytier-core|easytier-cli) ;;
    *)
      die "unsupported binary: $1"
      ;;
  esac
}

# Convert a Rust target triple into a shell-safe environment key suffix.
target_to_env_key() {
  printf '%s' "$1" | tr '[:lower:]-' '[:upper:]_'
}

# Convert a Rust target triple into a cargo env suffix that keeps lowercase letters.
target_to_var_key() {
  printf '%s' "$1" | tr '-' '_'
}

# Join arguments with a single space.
join_by_space() {
  local output=""
  local item
  for item in "$@"; do
    if [[ -n "$output" ]]; then
      output+=" "
    fi
    output+="$item"
  done
  printf '%s' "$output"
}

# Return the allocator feature that the CI uses for the selected target.
default_official_features() {
  local target="$1"
  if [[ "$target" =~ ^(riscv64|loongarch64|aarch64).* ]] || [[ "$target" =~ (freebsd|windows) ]]; then
    printf 'mimalloc'
  else
    printf 'jemalloc'
  fi
}

# Read a menu choice within the expected numeric range, supporting an optional default.
prompt_menu_index() {
  local title="$1"
  local count="$2"
  local default_value="${3:-0}"
  local choice=""
  local hint=""

  if (( default_value >= 1 && default_value <= count )); then
    hint=" [${default_value}]"
  fi

  while true; do
    ui_print "${COLOR_BOLD}${title}${hint}${COLOR_RESET}"
    read -r -p "请输入编号: " choice
    if [[ -z "$choice" ]] && (( default_value >= 1 && default_value <= count )); then
      printf '%s' "$default_value"
      return 0
    fi
    if [[ "$choice" =~ ^[0-9]+$ ]] && (( choice >= 1 && choice <= count )); then
      printf '%s' "$choice"
      return 0
    fi
    ui_print "${COLOR_YELLOW}[WARN]${COLOR_RESET} 请输入 1 到 ${count} 之间的编号"
  done
}

# Let the user choose one item from a fixed option list.
prompt_select() {
  local title="$1"
  shift
  local options=("$@")
  local idx=1
  local choice
  local option

  for option in "${options[@]}"; do
    printf '  %s%d%s) %s\n' "${COLOR_CYAN}" "$idx" "${COLOR_RESET}" "$option" >&2
    ((idx++))
  done

  choice="$(prompt_menu_index "$title" "${#options[@]}" 1)"
  printf '%s' "${options[choice-1]}"
}

# Ask a yes or no question with a default choice.
prompt_yes_no() {
  local title="$1"
  local default_value="$2"
  local answer=""
  local prompt="[y/N]"

  if [[ "$default_value" == "yes" ]]; then
    prompt="[Y/n]"
  fi

  while true; do
    printf '%s%s%s %s ' "${COLOR_BOLD}" "${title}" "${COLOR_RESET}" "${prompt}" >&2
    read -r answer
    if [[ -z "$answer" ]]; then
      printf '%s' "$default_value"
      return 0
    fi

    case "$answer" in
      y|Y|yes|YES)
        printf 'yes'
        return 0
        ;;
      n|N|no|NO)
        printf 'no'
        return 0
        ;;
      *)
        ui_print "${COLOR_YELLOW}[WARN]${COLOR_RESET} 请输入 y 或 n"
        ;;
    esac
  done
}

# Ask for free-form input and allow an empty default value.
prompt_input() {
  local title="$1"
  local default_value="$2"
  local answer=""

  if [[ -n "$default_value" ]]; then
    printf '%s%s%s [%s]: ' "${COLOR_BOLD}" "${title}" "${COLOR_RESET}" "$default_value" >&2
  else
    printf '%s%s%s: ' "${COLOR_BOLD}" "${title}" "${COLOR_RESET}" >&2
  fi

  read -r answer
  if [[ -z "$answer" ]]; then
    printf '%s' "$default_value"
  else
    printf '%s' "$answer"
  fi
}

# Run an interactive menu to populate build options.
interactive_menu() {
  local host_target="$1"
  local target_choice
  local custom_target=""
  local clean_answer
  local offline_answer
  local upx_answer
  local confirm_answer
  local feature_default=""

  printf '%sEasyTier Build Menu%s\n' "${COLOR_BOLD}${COLOR_CYAN}" "${COLOR_RESET}"
  printf '  Host target: %s%s%s\n' "${COLOR_GREEN}" "$host_target" "${COLOR_RESET}"
  printf '  Toolchain: %s%s%s\n\n' "${COLOR_GREEN}" "$TOOLCHAIN" "${COLOR_RESET}"

  BIN_NAME="$(prompt_select "请选择项目" "easytier-core" "easytier-cli")"
  printf '\n'
  METHOD="$(prompt_select "请选择编译方式" "debug" "release" "release-small" "official")"
  printf '\n'

  printf '  %s1%s) host (%s)\n' "${COLOR_CYAN}" "${COLOR_RESET}" "$host_target"
  printf '  %s2%s) x86_64-pc-windows-msvc\n' "${COLOR_CYAN}" "${COLOR_RESET}"
  printf '  %s3%s) x86_64-pc-windows-gnu\n' "${COLOR_CYAN}" "${COLOR_RESET}"
  printf '  %s4%s) x86_64-unknown-linux-musl\n' "${COLOR_CYAN}" "${COLOR_RESET}"
  printf '  %s5%s) aarch64-unknown-linux-musl\n' "${COLOR_CYAN}" "${COLOR_RESET}"
  printf '  %s6%s) 自定义 target\n' "${COLOR_CYAN}" "${COLOR_RESET}"
  target_choice="$(prompt_menu_index "请选择目标平台" 6 1)"

  case "$target_choice" in
    1) TARGET="$host_target" ;;
    2) TARGET="x86_64-pc-windows-msvc" ;;
    3) TARGET="x86_64-pc-windows-gnu" ;;
    4) TARGET="x86_64-unknown-linux-musl" ;;
    5) TARGET="aarch64-unknown-linux-musl" ;;
    6)
      custom_target="$(prompt_input "请输入自定义 target triple" "")"
      [[ -n "$custom_target" ]] || die "target 不能为空"
      TARGET="$custom_target"
      ;;
  esac

  printf '\n'
  clean_answer="$(prompt_yes_no "构建前是否清理已有产物？" "no")"
  [[ "$clean_answer" == "yes" ]] && CLEAN_BEFORE_BUILD=1 || CLEAN_BEFORE_BUILD=0

  offline_answer="$(prompt_yes_no "是否使用离线模式？" "yes")"
  [[ "$offline_answer" == "yes" ]] && OFFLINE_BUILD=1 || OFFLINE_BUILD=0

  if [[ "$METHOD" == "official" ]]; then
    printf '\n'
    upx_answer="$(prompt_select "官方模式是否启用 UPX 压缩" "auto" "yes" "no")"
    USE_UPX="$upx_answer"
    feature_default="$(default_official_features "$TARGET")"
  fi

  printf '\n'
  FEATURES="$(prompt_input "请输入 features（留空使用默认）" "$feature_default")"

  printf '\n'
  log_step "已选择配置"
  log_info "method=${METHOD}"
  log_info "bin=${BIN_NAME}"
  log_info "target=${TARGET}"
  [[ -n "$FEATURES" ]] && log_info "features=${FEATURES}" || log_info "features=<default>"
  [[ "$METHOD" == "official" ]] && log_info "upx=${USE_UPX}"
  [[ "$CLEAN_BEFORE_BUILD" -eq 1 ]] && log_info "clean=yes" || log_info "clean=no"
  [[ "$OFFLINE_BUILD" -eq 1 ]] && log_info "offline=yes" || log_info "offline=no"
  printf '\n'

  confirm_answer="$(prompt_yes_no "确认以上配置并开始构建？" "yes")"
  if [[ "$confirm_answer" != "yes" ]]; then
    log_warn "已取消构建"
    exit 0
  fi
  printf '\n'
}

# Configure cross-compilation environment variables for musl targets when a local cross toolchain is available.
configure_target_env() {
  local target="$1"
  local target_env_key
  local target_var_key
  local musl_triplet
  local linker=""
  local ar=""
  local sysroot=""
  local gcc_include=""
  local clang_args=()

  target_env_key="$(target_to_env_key "$target")"
  target_var_key="$(target_to_var_key "$target")"

  if [[ -n "$PROTOC_BIN" ]]; then
    BUILD_ENV+=("PROTOC=$PROTOC_BIN")
  fi

  if [[ "$target" != *-unknown-linux-musl ]]; then
    return 0
  fi

  musl_triplet="${target/-unknown-linux-musl/-linux-musl}"
  linker="$(command -v "${musl_triplet}-gcc" || true)"
  ar="$(command -v "${musl_triplet}-ar" || true)"

  if [[ -z "$linker" ]]; then
    log_warn "未找到 ${musl_triplet}-gcc，cargo 将使用默认 linker"
    return 0
  fi

  BUILD_ENV+=("CARGO_TARGET_${target_env_key}_LINKER=$linker")
  BUILD_ENV+=("CC_${target_var_key}=$linker")

  if [[ -n "$ar" ]]; then
    BUILD_ENV+=("AR_${target_var_key}=$ar")
  fi

  sysroot="$("$linker" -print-sysroot 2>/dev/null || true)"
  gcc_include="$("$linker" -print-file-name=include 2>/dev/null || true)"

  if [[ -n "$sysroot" ]]; then
    clang_args+=("--sysroot=$sysroot")
    [[ -d "$sysroot/include" ]] && clang_args+=("-isystem" "$sysroot/include")
    [[ -d "$sysroot/usr/include" ]] && clang_args+=("-isystem" "$sysroot/usr/include")
  fi

  if [[ -n "$gcc_include" && -d "$gcc_include" ]]; then
    clang_args+=("-isystem" "$gcc_include")
  fi

  if [[ ${#clang_args[@]} -gt 0 ]]; then
    BUILD_ENV+=("BINDGEN_EXTRA_CLANG_ARGS_${target_var_key}=$(join_by_space "${clang_args[@]}")")
  fi
}

# Add method-specific cargo arguments and build environment variables.
configure_build_mode() {
  local host_target="$1"

  if [[ -z "$TARGET" ]]; then
    TARGET="$host_target"
  fi

  case "$METHOD" in
    debug)
      ;;
    release)
      EXTRA_CARGO_ARGS+=(--release)
      ;;
    release-small)
      EXTRA_CARGO_ARGS+=(--profile release-small)
      BUILD_ENV+=("RUSTFLAGS=${RUSTFLAGS:-} -C link-arg=-Wl,--gc-sections")
      ;;
    official)
      EXTRA_CARGO_ARGS+=(--release)
      if [[ -z "$FEATURES" ]]; then
        FEATURES="$(default_official_features "$TARGET")"
      fi
      if [[ "$USE_UPX" == "auto" ]]; then
        USE_UPX="yes"
      fi
      ;;
  esac
}

# Clean the selected easytier artifacts without deleting the whole target directory tree.
clean_previous_outputs() {
  local cargo_clean_cmd=(cargo "+${TOOLCHAIN}" clean -p easytier --target "$TARGET")

  log_step "清理旧产物"
  "${cargo_clean_cmd[@]}"

  if [[ "$METHOD" == "official" ]]; then
    local artifact_dir
    artifact_dir="$(official_artifact_dir)"
    rm -f "${artifact_dir}/${BIN_NAME}$(binary_suffix)"
  fi
}

# Return the cargo output directory for the current method and target.
cargo_output_dir() {
  case "$METHOD" in
    debug)
      printf '%s/target/%s/debug' "$ROOT_DIR" "$TARGET"
      ;;
    release|official)
      printf '%s/target/%s/release' "$ROOT_DIR" "$TARGET"
      ;;
    release-small)
      printf '%s/target/%s/release-small' "$ROOT_DIR" "$TARGET"
      ;;
  esac
}

# Return the artifact directory used by the official packaging mode.
official_artifact_dir() {
  case "$TARGET" in
    x86_64-unknown-linux-musl)
      printf '%s/target/easytier-linux-x86_64' "$ROOT_DIR"
      ;;
    aarch64-unknown-linux-musl)
      printf '%s/target/easytier-linux-aarch64' "$ROOT_DIR"
      ;;
    x86_64-pc-windows-gnu|x86_64-pc-windows-msvc)
      printf '%s/target/easytier-windows-x86_64' "$ROOT_DIR"
      ;;
    *)
      printf '%s/target/official-%s' "$ROOT_DIR" "$TARGET"
      ;;
  esac
}

# Return the executable suffix for the selected target.
binary_suffix() {
  case "$TARGET" in
    *windows*)
      printf '.exe'
      ;;
    *)
      printf ''
      ;;
  esac
}

# Return the full path of the built binary in the cargo target directory.
built_binary_path() {
  printf '%s/%s%s' "$(cargo_output_dir)" "$BIN_NAME" "$(binary_suffix)"
}

# Optionally compress the built artifact with UPX to match the CI packaging flow.
compress_with_upx_if_needed() {
  local binary_path="$1"
  local upx_bin=""

  if [[ "$METHOD" != "official" ]]; then
    return 0
  fi

  if [[ "$USE_UPX" == "no" ]]; then
    log_info "UPX compression disabled"
    return 0
  fi

  upx_bin="$(command -v upx || true)"
  if [[ -z "$upx_bin" ]]; then
    if [[ "$USE_UPX" == "yes" ]]; then
      log_warn "UPX not found in PATH, skipping compression"
    else
      log_info "UPX not found in PATH, skipping compression"
    fi
    return 0
  fi

  log_step "使用 UPX 压缩产物"
  if ! "$upx_bin" --lzma --best "$binary_path"; then
    log_warn "UPX compression failed, continuing..."
  fi
}

# Copy the official build result into a stable artifact directory.
copy_official_artifact() {
  local binary_path="$1"
  local artifact_dir

  if [[ "$METHOD" != "official" ]]; then
    return 0
  fi

  artifact_dir="$(official_artifact_dir)"
  mkdir -p "$artifact_dir"
  cp "$binary_path" "${artifact_dir}/${BIN_NAME}$(binary_suffix)"
  log_success "已复制到 ${artifact_dir}/${BIN_NAME}$(binary_suffix)"
}

# Print the final artifact path and size so the caller can find the result quickly.
print_result_summary() {
  local binary_path="$1"
  local final_path="$binary_path"
  local size_bytes=""
  local size_str=""

  if [[ "$METHOD" == "official" ]]; then
    final_path="$(official_artifact_dir)/${BIN_NAME}$(binary_suffix)"
  fi

  size_bytes="$(stat -f '%z' "$final_path")"
  if (( size_bytes >= 1048576 )); then
    size_str="$(awk -v b="$size_bytes" 'BEGIN {printf "%.2f MB", b/1048576}')"
  elif (( size_bytes >= 1024 )); then
    size_str="$(awk -v b="$size_bytes" 'BEGIN {printf "%.2f KB", b/1024}')"
  else
    size_str="${size_bytes} bytes"
  fi

  printf '\n%sBuild Result%s\n' "${COLOR_BOLD}${COLOR_GREEN}" "${COLOR_RESET}"
  printf '  Path: %s\n' "$final_path"
  printf '  Size: %s\n' "$size_str"
}

init_colors

ORIGINAL_ARG_COUNT=$#

while [[ $# -gt 0 ]]; do
  case "$1" in
    -m|--method)
      [[ $# -ge 2 ]] || die "missing value for $1"
      METHOD="$2"
      shift 2
      ;;
    -b|--bin)
      [[ $# -ge 2 ]] || die "missing value for $1"
      BIN_NAME="$2"
      shift 2
      ;;
    -t|--target)
      [[ $# -ge 2 ]] || die "missing value for $1"
      TARGET="$2"
      shift 2
      ;;
    --features)
      [[ $# -ge 2 ]] || die "missing value for $1"
      FEATURES="$2"
      shift 2
      ;;
    --clean)
      CLEAN_BEFORE_BUILD=1
      shift
      ;;
    --offline)
      OFFLINE_BUILD=1
      shift
      ;;
    --upx)
      USE_UPX="yes"
      shift
      ;;
    --no-upx)
      USE_UPX="no"
      shift
      ;;
    --menu)
      MENU_MODE=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

validate_method "$METHOD"
validate_bin "$BIN_NAME"

[[ -f "${ROOT_DIR}/Cargo.toml" ]] || die "Cargo.toml not found in ${ROOT_DIR}"

HOST_TARGET="$(detect_host_target)"

if [[ "$ORIGINAL_ARG_COUNT" -eq 0 ]]; then
  MENU_MODE=1
fi

if [[ "$MENU_MODE" -eq 1 ]]; then
  interactive_menu "$HOST_TARGET"
fi

if PROTOC_BIN="$(detect_protoc)"; then
  log_info "detected protoc=${PROTOC_BIN}"
elif [[ "$MENU_MODE" -eq 1 ]]; then
  log_warn "未检测到 protoc，后续真正构建时会报错；可先设置 PROTOC=/path/to/protoc"
fi

ensure_protoc

configure_build_mode "$HOST_TARGET"
configure_target_env "$TARGET"

if [[ "$CLEAN_BEFORE_BUILD" -eq 1 ]]; then
  clean_previous_outputs
fi

CARGO_CMD=(cargo "+${TOOLCHAIN}" build -p easytier --bin "$BIN_NAME" --target "$TARGET")

if [[ -n "$FEATURES" ]]; then
  CARGO_CMD+=(--features "$FEATURES")
fi

if [[ "$OFFLINE_BUILD" -eq 1 ]]; then
  CARGO_CMD+=(--offline)
fi

if [[ ${#EXTRA_CARGO_ARGS[@]} -gt 0 ]]; then
  CARGO_CMD+=("${EXTRA_CARGO_ARGS[@]}")
fi

log_step "开始构建"
log_info "method=${METHOD}"
log_info "bin=${BIN_NAME}"
log_info "target=${TARGET}"
[[ -n "$FEATURES" ]] && log_info "features=${FEATURES}" || log_info "features=<default>"
log_info "protoc=${PROTOC_BIN}"
[[ "$METHOD" == "official" ]] && log_info "upx=${USE_UPX}"
log_info "running: ${CARGO_CMD[*]}"

if [[ ${#BUILD_ENV[@]} -gt 0 ]]; then
  env "${BUILD_ENV[@]}" "${CARGO_CMD[@]}"
else
  "${CARGO_CMD[@]}"
fi

BUILT_BINARY="$(built_binary_path)"
[[ -f "$BUILT_BINARY" ]] || die "build finished but binary not found: ${BUILT_BINARY}"

compress_with_upx_if_needed "$BUILT_BINARY"
copy_official_artifact "$BUILT_BINARY"
print_result_summary "$BUILT_BINARY"
