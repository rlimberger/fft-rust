#!/usr/bin/env bash
# perf-runner/provision.sh — FFT perf-runner provisioning kit (spec: docs/PERF-RUNNER.md).
#
# The perf runner is the dev box itself (Arch Linux, Hyprland/Wayland), registered as a
# GitHub Actions self-hosted runner with labels `self-hosted,fft-perf` for
# github.com/rlimberger/fft-rust. Safe to re-run (idempotent). Fails loudly: no silent
# fallbacks, no defaulted values when detection fails.
#
# Usage:
#   ./provision.sh manifest
#       Detect machine configuration and write perf-runner/manifest.json (committed per
#       spec). Read-only, no sudo. If ANY component fails detection, the failures are
#       listed on stderr, the partial JSON (failed fields marked "DETECTION-FAILED") is
#       printed for inspection, manifest.json is NOT written, and the exit code is 1.
#
#   sudo ./provision.sh apply <github-runner-token> [--pin-packages]
#       Requires root via sudo (runner registration runs as the invoking user).
#       1. Installs+enables fft-perf-governor.service (cpupower governor=performance on boot).
#       2. Downloads the latest GitHub Actions runner into perf-runner/actions-runner/,
#          registers it for github.com/rlimberger/fft-rust with label fft-perf
#          (self-hosted is added by GitHub automatically), installs it as a systemd
#          service running as the invoking user.
#          Token: repo Settings -> Actions -> Runners -> New self-hosted runner.
#       3. Pinned-package policy (kernel + detected GPU driver packages): only appended to
#          /etc/pacman.conf IgnorePkg with --pin-packages; otherwise the exact line is
#          printed for manual application.
#
#   ./provision.sh preflight
#       Thermal/isolation precondition for measured runs (called by the perf workflow
#       before gates): governor=performance on all cores, no other CI job running, a
#       240 Hz display mode active (prints the exact hyprland `monitor=` line to add if
#       not — this script never edits ~/.config), then 60 s idle wait, then package
#       temperature below manifest.json thermal_threshold_c (default 55 C). Exit 0/1.
#
# Hyprland config is user-managed: this script never writes under ~/.config.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$SCRIPT_DIR/manifest.json"
RUNNER_DIR="$SCRIPT_DIR/actions-runner"
REPO_URL="https://github.com/rlimberger/fft-rust"
RUNNER_LABEL="fft-perf"
GOVERNOR_UNIT="fft-perf-governor.service"
THERMAL_THRESHOLD_C=55
IDLE_WAIT_S=60

die() { echo "ERROR: $*" >&2; exit 1; }
info() { echo "[provision] $*"; }

need() { command -v "$1" >/dev/null 2>&1 || die "required tool missing: $1 ($2)"; }

# ---------------------------------------------------------------------------
# hyprctl needs the Hyprland instance socket. Under a systemd service (the CI
# runner) HYPRLAND_INSTANCE_SIGNATURE is not inherited, so discover it.
# ---------------------------------------------------------------------------
hypr() {
    if [[ -z "${HYPRLAND_INSTANCE_SIGNATURE:-}" ]]; then
        local rt="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
        [[ -d "$rt/hypr" ]] || { echo "no Hyprland instance dir at $rt/hypr" >&2; return 1; }
        local sig
        sig="$(ls -t "$rt/hypr" 2>/dev/null | head -n1)"
        [[ -n "$sig" ]] || { echo "no Hyprland instance under $rt/hypr" >&2; return 1; }
        XDG_RUNTIME_DIR="$rt" HYPRLAND_INSTANCE_SIGNATURE="$sig" hyprctl "$@"
    else
        hyprctl "$@"
    fi
}

# ---------------------------------------------------------------------------
# GPU detection (shared by manifest + apply)
# ---------------------------------------------------------------------------
gpu_lspci_line() {
    lspci | grep -Ei 'vga compatible controller|3d controller|display controller' | head -n1
}

gpu_vendor() {
    local line
    line="$(gpu_lspci_line)" || return 1
    case "$line" in
        *AMD*|*ATI*)  echo amd ;;
        *NVIDIA*)     echo nvidia ;;
        *Intel*)      echo intel ;;
        *)            return 1 ;;
    esac
}

# Installed packages whose upgrade would change the pinned GPU/kernel stack.
pin_packages() {
    local vendor="$1" candidates=() p out=()
    case "$vendor" in
        amd)    candidates=(mesa vulkan-radeon lib32-mesa libva-mesa-driver) ;;
        nvidia) candidates=(nvidia nvidia-lts nvidia-utils lib32-nvidia-utils) ;;
        intel)  candidates=(mesa vulkan-intel lib32-mesa) ;;
        *)      return 1 ;;
    esac
    for p in linux linux-lts linux-zen linux-hardened "${candidates[@]}"; do
        pacman -Q "$p" >/dev/null 2>&1 && out+=("$p")
    done
    [[ ${#out[@]} -gt 0 ]] || return 1
    echo "${out[@]}"
}

gpu_driver_version() {
    local vendor="$1" slot kdrv userspace
    slot="$(gpu_lspci_line | cut -d' ' -f1)"
    kdrv="$(lspci -ks "$slot" | awk -F': ' '/Kernel driver in use/{print $2}')"
    [[ -n "$kdrv" ]] || return 1
    case "$vendor" in
        amd|intel) userspace="$(pacman -Q mesa)" ;;
        nvidia)    userspace="$(pacman -Q nvidia-utils 2>/dev/null || pacman -Q nvidia)" ;;
    esac
    [[ -n "$userspace" ]] || return 1
    echo "$kdrv (kernel) + $userspace"
}

# ---------------------------------------------------------------------------
# manifest
# ---------------------------------------------------------------------------
cmd_manifest() {
    need lspci "pacman -S pciutils"
    need jq "pacman -S jq"
    local -a FAILS=()
    fail_component() { FAILS+=("$1"); echo "ERROR: detection failed: $1" >&2; }

    local cpu_model cores_logical cores_physical smt governor
    local gpu_vendor_v gpu_model gpu_driver kernel hypr_version monitors_json
    local rustc_v cargo_v host now

    cpu_model="$(awk -F': ' '/^model name/{print $2; exit}' /proc/cpuinfo || true)"
    [[ -n "$cpu_model" ]] || { fail_component cpu.model; cpu_model="DETECTION-FAILED"; }

    cores_logical="$(nproc --all || true)"
    [[ -n "$cores_logical" ]] || { fail_component cpu.cores_logical; cores_logical=0; }

    cores_physical="$(awk '/^cpu cores/{print $4; exit}' /proc/cpuinfo || true)"
    [[ -n "$cores_physical" ]] || { fail_component cpu.cores_physical; cores_physical=0; }

    smt="$(cat /sys/devices/system/cpu/smt/control 2>/dev/null || true)"
    [[ -n "$smt" ]] || { fail_component cpu.smt; smt="DETECTION-FAILED"; }

    governor="$(sort -u /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor 2>/dev/null \
        | paste -sd, - || true)"
    [[ -n "$governor" ]] || { fail_component cpu.governor; governor="DETECTION-FAILED"; }

    gpu_vendor_v="$(gpu_vendor || true)"
    [[ -n "$gpu_vendor_v" ]] || { fail_component gpu.vendor; gpu_vendor_v="DETECTION-FAILED"; }

    gpu_model="$(gpu_lspci_line | sed 's/^[0-9a-f:.]* //' || true)"
    [[ -n "$gpu_model" ]] || { fail_component gpu.model; gpu_model="DETECTION-FAILED"; }

    gpu_driver="DETECTION-FAILED"
    if [[ "$gpu_vendor_v" != "DETECTION-FAILED" ]]; then
        gpu_driver="$(gpu_driver_version "$gpu_vendor_v" || true)"
        [[ -n "$gpu_driver" ]] || { fail_component gpu.driver; gpu_driver="DETECTION-FAILED"; }
    else
        fail_component gpu.driver
    fi

    kernel="$(uname -r || true)"
    [[ -n "$kernel" ]] || { fail_component kernel; kernel="DETECTION-FAILED"; }

    hypr_version="$(hypr version 2>/dev/null | head -n1 || true)"
    [[ "$hypr_version" == Hyprland* ]] || { fail_component hyprland.version; hypr_version="DETECTION-FAILED"; }

    monitors_json="$(hypr -j monitors 2>/dev/null \
        | jq -c '[.[] | {name, description, width, height, refreshRate, scale}]' 2>/dev/null || true)"
    [[ -n "$monitors_json" && "$monitors_json" != "[]" ]] \
        || { fail_component monitors; monitors_json='"DETECTION-FAILED"'; }

    rustc_v="$(rustc -V 2>/dev/null || true)"
    [[ -n "$rustc_v" ]] || { fail_component rustc; rustc_v="DETECTION-FAILED"; }

    cargo_v="$(cargo -V 2>/dev/null || true)"
    [[ -n "$cargo_v" ]] || { fail_component cargo; cargo_v="DETECTION-FAILED"; }

    host="$(hostname)"
    now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    local json
    json="$(jq -n \
        --arg hostname "$host" \
        --arg date "$now" \
        --arg cpu_model "$cpu_model" \
        --argjson cores_logical "$cores_logical" \
        --argjson cores_physical "$cores_physical" \
        --arg smt "$smt" \
        --arg governor "$governor" \
        --arg gpu_vendor "$gpu_vendor_v" \
        --arg gpu_model "$gpu_model" \
        --arg gpu_driver "$gpu_driver" \
        --arg kernel "$kernel" \
        --arg hyprland "$hypr_version" \
        --argjson monitors "$monitors_json" \
        --arg rustc "$rustc_v" \
        --arg cargo "$cargo_v" \
        --argjson threshold "$THERMAL_THRESHOLD_C" \
        '{
            hostname: $hostname,
            date: $date,
            cpu: { model: $cpu_model, cores_logical: $cores_logical,
                   cores_physical: $cores_physical, smt: $smt, governor: $governor },
            gpu: { vendor: $gpu_vendor, model: $gpu_model, driver: $gpu_driver },
            kernel: $kernel,
            hyprland: $hyprland,
            monitors: $monitors,
            rust: { rustc: $rustc, cargo: $cargo },
            thermal_threshold_c: $threshold
        }')"

    if [[ ${#FAILS[@]} -gt 0 ]]; then
        echo "$json"
        echo "ERROR: ${#FAILS[@]} component(s) failed detection: ${FAILS[*]}" >&2
        echo "ERROR: manifest.json NOT written (a committed manifest must be complete)." >&2
        exit 1
    fi
    echo "$json" > "$MANIFEST"
    echo "$json"
    info "wrote $MANIFEST"
}

# ---------------------------------------------------------------------------
# apply
# ---------------------------------------------------------------------------
cmd_apply() {
    [[ $EUID -eq 0 ]] || die "apply requires root: sudo $0 apply <token> [--pin-packages]"
    [[ -n "${SUDO_USER:-}" && "$SUDO_USER" != root ]] \
        || die "run via sudo from your user account (SUDO_USER must be a non-root user)"
    local token="${1:-}" pin=0
    [[ -n "$token" ]] || die "missing <github-runner-token>. Get one at: $REPO_URL/settings/actions/runners/new"
    [[ "${2:-}" == "--pin-packages" ]] && pin=1

    need jq "pacman -S jq"
    need curl "pacman -S curl"
    need cpupower "pacman -S cpupower"

    # 1. governor=performance on boot -------------------------------------
    info "installing $GOVERNOR_UNIT"
    cat > "/etc/systemd/system/$GOVERNOR_UNIT" <<EOF
[Unit]
Description=FFT perf runner: CPU governor = performance (docs/PERF-RUNNER.md)
After=multi-user.target

[Service]
Type=oneshot
ExecStart=/usr/bin/cpupower frequency-set -g performance
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable --now "$GOVERNOR_UNIT"
    info "$GOVERNOR_UNIT enabled and started"

    # 2. GitHub Actions runner ---------------------------------------------
    if [[ -f "$RUNNER_DIR/.runner" ]]; then
        info "runner already registered ($RUNNER_DIR/.runner exists) — skipping download/config"
    else
        info "downloading latest actions/runner release"
        local api asset
        api="$(curl -fsSL https://api.github.com/repos/actions/runner/releases/latest)" \
            || die "could not query actions/runner latest release"
        asset="$(echo "$api" | jq -r \
            '.assets[] | select(.name | test("^actions-runner-linux-x64-[0-9.]+\\.tar\\.gz$")) | .browser_download_url')"
        [[ -n "$asset" ]] || die "no linux-x64 asset in latest actions/runner release"
        runuser -u "$SUDO_USER" -- mkdir -p "$RUNNER_DIR"
        runuser -u "$SUDO_USER" -- curl -fL -o "$RUNNER_DIR/runner.tar.gz" "$asset"
        runuser -u "$SUDO_USER" -- tar xzf "$RUNNER_DIR/runner.tar.gz" -C "$RUNNER_DIR"
        info "registering runner for $REPO_URL (labels: self-hosted,$RUNNER_LABEL)"
        ( cd "$RUNNER_DIR" && runuser -u "$SUDO_USER" -- ./config.sh \
            --url "$REPO_URL" --token "$token" --name "$(hostname)" \
            --labels "$RUNNER_LABEL" --work _work --unattended --replace )
    fi

    if [[ -f "$RUNNER_DIR/.service" ]]; then
        info "runner systemd service already installed — skipping svc.sh install"
    else
        ( cd "$RUNNER_DIR" && ./svc.sh install "$SUDO_USER" )
    fi
    ( cd "$RUNNER_DIR" && ./svc.sh start ) || true
    ( cd "$RUNNER_DIR" && ./svc.sh status ) || true

    # 3. pinned-package policy ----------------------------------------------
    local vendor pkgs line
    vendor="$(gpu_vendor)" || die "cannot detect GPU vendor for package pinning"
    pkgs="$(pin_packages "$vendor")" || die "no kernel/GPU driver packages found to pin"
    line="IgnorePkg = $pkgs"
    if [[ $pin -eq 1 ]]; then
        apply_ignorepkg "$pkgs"
    else
        info "package pinning NOT applied (pass --pin-packages to apply). Manual line for /etc/pacman.conf [options]:"
        echo "$line"
    fi
    info "apply done"
}

apply_ignorepkg() {
    local pkgs="$1" conf=/etc/pacman.conf p missing=()
    local current
    current="$(grep -E '^\s*IgnorePkg\s*=' "$conf" | sed 's/^[^=]*=//' || true)"
    for p in $pkgs; do
        grep -qw "$p" <<< "$current" || missing+=("$p")
    done
    if [[ ${#missing[@]} -eq 0 ]]; then
        info "IgnorePkg already covers: $pkgs"
        return 0
    fi
    cp -n "$conf" "${conf}.fft-perf.bak"
    if grep -qE '^\s*IgnorePkg\s*=' "$conf"; then
        sed -i "0,/^\s*IgnorePkg\s*=.*/s//& ${missing[*]}/" "$conf"
    else
        sed -i "/^\[options\]/a IgnorePkg = ${missing[*]}" "$conf"
    fi
    info "appended to $conf IgnorePkg: ${missing[*]} (backup: ${conf}.fft-perf.bak)"
}

# ---------------------------------------------------------------------------
# preflight
# ---------------------------------------------------------------------------
read_package_temp_c() {
    local t
    if command -v sensors >/dev/null 2>&1; then
        t="$(sensors -j 2>/dev/null | jq -r \
            '[.[] | select(type=="object")
              | (.Tctl.temp1_input? // ."Package id 0".temp1_input?)
              | select(. != null)] | first // empty')"
        [[ -n "$t" ]] && { echo "$t"; return 0; }
    fi
    local z
    for z in /sys/class/thermal/thermal_zone*; do
        [[ -e "$z/type" ]] || continue
        if [[ "$(cat "$z/type")" == "x86_pkg_temp" ]]; then
            awk '{printf "%.1f", $1/1000}' "$z/temp"
            return 0
        fi
    done
    echo "ERROR: no package temperature source (k10temp Tctl / coretemp Package via sensors -j, or x86_pkg_temp thermal zone)" >&2
    return 1
}

cmd_preflight() {
    need jq "pacman -S jq"
    local rc=0

    [[ -f "$MANIFEST" ]] || die "no $MANIFEST — run '$0 manifest' first (gate results without a manifest are invalid)"
    local threshold
    threshold="$(jq -r ".thermal_threshold_c // $THERMAL_THRESHOLD_C" "$MANIFEST")"

    # governor = performance on every core
    local bad
    bad="$(grep -Lx performance /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor || true)"
    if [[ -n "$bad" ]]; then
        echo "FAIL: governor != performance on: $bad" >&2
        echo "      fix: sudo systemctl start $GOVERNOR_UNIT  (or: sudo cpupower frequency-set -g performance)" >&2
        rc=1
    else
        info "governor: performance on all cores"
    fi

    # no other CI job running (one Runner.Worker = the job invoking us)
    local workers
    workers="$(pgrep -c -f Runner.Worker || true)"
    if [[ "${workers:-0}" -gt 1 ]]; then
        echo "FAIL: $workers CI jobs running (Runner.Worker) — perf gates require exclusive use" >&2
        rc=1
    else
        info "CI isolation: ${workers:-0} Runner.Worker process(es)"
    fi

    # Active display modes must match the committed manifest — drift invalidates results.
    # 240 Hz hardware is mandatory from the M3 gate onward; until then the manifest rules.
    local monitors want have
    if monitors="$(hypr -j monitors 2>/dev/null)" && [[ -n "$monitors" ]]; then
        want="$(jq -cS '[.monitors[] | {name, width, height, hz: (.refreshRate | round)}]' "$MANIFEST")"
        have="$(echo "$monitors" | jq -cS '[.[] | {name, width, height, hz: (.refreshRate | round)}]')"
        if [[ "$want" == "$have" ]]; then
            info "display: active modes match manifest"
            if echo "$monitors" | jq -e '[.[] | select(.refreshRate >= 239)] | length == 0' >/dev/null; then
                info "display: no 240 Hz mode attached (required from the M3 gate onward)"
            fi
        else
            echo "FAIL: active display modes differ from manifest.json:" >&2
            echo "  manifest: $want" >&2
            echo "  active:   $have" >&2
            echo "      if the change is intended, re-run '$0 manifest' and commit the new manifest" >&2
            rc=1
        fi
    else
        echo "FAIL: cannot query monitors via hyprctl (is Hyprland running for this user?)" >&2
        rc=1
    fi

    [[ $rc -eq 0 ]] || { echo "preflight FAILED before idle wait" >&2; exit 1; }

    info "idle wait: ${IDLE_WAIT_S}s (thermal settle)"
    sleep "$IDLE_WAIT_S"

    local temp
    temp="$(read_package_temp_c)" || exit 1
    if awk -v t="$temp" -v th="$threshold" 'BEGIN{exit !(t < th)}'; then
        info "package temp: ${temp}C < ${threshold}C"
    else
        echo "FAIL: package temp ${temp}C >= threshold ${threshold}C" >&2
        exit 1
    fi

    info "preflight PASSED"
}

# ---------------------------------------------------------------------------
usage() { awk 'NR>1 && !/^#/{exit} NR>1{sub(/^# ?/,""); print}' "${BASH_SOURCE[0]}"; }

case "${1:-}" in
    manifest)  cmd_manifest ;;
    apply)     shift; cmd_apply "$@" ;;
    preflight) cmd_preflight ;;
    -h|--help|help|"") usage; [[ "${1:-}" ]] || exit 1 ;;
    *) die "unknown subcommand: $1 (see --help)" ;;
esac
