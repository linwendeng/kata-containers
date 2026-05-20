# Kata-dev Test Environment (NVIDIA GB200 VFIO Passthrough)

This document describes how to bring up a host for developing and testing the
locally built `runtime-rs` Kata shim against NVIDIA Grace-Blackwell (GB200 /
GH200) GPUs using VFIO passthrough.

The environment is structured so that:

* A **release** Kata install (`/opt/kata`) is kept untouched and serves as a
  fallback runtime class (`kata-qemu-nvidia-gpu`).
* A **dev** runtime class (`kata-qemu-nvidia-gpu-dev`) points at the
  freshly compiled shim under `target/` so you can iterate without
  reinstalling the kata-deploy bundle.

---

## 1. Host requirements

| Item | Tested value |
|---|---|
| Arch | aarch64 (Grace) |
| OS | Ubuntu 24.04 (Noble) |
| Kernel | 6.17.x with `iommu.passthrough=0 iommu=pt vfio_iommu_type1.allow_unsafe_interrupts=1` |
| GPUs | 4 × NVIDIA GB200 (`10de:2941`) bound to `nvgrace_gpu_vfio_pci` |
| NICs | 4 × Mellanox CX (`15b3:2100`) bound to `vfio-pci` |
| QEMU | kata-static QEMU 10.2+ at `/opt/kata/bin/qemu-system-aarch64` |
| containerd | 2.x with CRI v1 + CDI enabled |
| Rust | toolchain from `rust-toolchain.toml` (musl target installed) |
| Host NVIDIA driver | 580.x (host-side, for SR-IOV / variant driver) |

Verify the GPU host binding:

```bash
lspci -nnk -d 10de:2941 | grep -E "in use|driver"
# expected: Kernel driver in use: nvgrace_gpu_vfio_pci
```

If the GPU is bound to `nvidia` instead, unbind it and bind the variant driver:

```bash
for bdf in 0008:06:00.0 0009:06:00.0 0018:06:00.0 0019:06:00.0; do
  echo $bdf > /sys/bus/pci/devices/$bdf/driver/unbind 2>/dev/null
  echo 10de 2941 > /sys/bus/pci/drivers/nvgrace_gpu_vfio_pci/new_id 2>/dev/null
done
```

---

## 2. Layout

```
/opt/kata-containers/        # source tree (this repo, on branch gb200-dev)
/opt/kata/                   # release kata-deploy install (untouched)
/opt/vm-images/              # libvirt reference VM artifacts (optional)
/etc/kata-containers/runtime-rs/configuration.toml   # dev shim config
/etc/containerd/conf.d/kata-dev.toml                 # dev runtime class
/etc/cdi/nvidia.yaml         # CDI map: gpu0..gpu3 -> host BDFs
```

---

## 3. Build the runtime-rs shim

```bash
cd /opt/kata-containers/src/runtime-rs
cargo build -p containerd-shim-kata-v2 \
    --release \
    --target aarch64-unknown-linux-musl
```

Built binary:

```
/opt/kata-containers/target/aarch64-unknown-linux-musl/release/containerd-shim-kata-v2
```

Quick sanity check that the GB200 patches are present:

```bash
strings target/aarch64-unknown-linux-musl/release/containerd-shim-kata-v2 \
  | grep -E "acpi-generic-initiator|vfio_gpu|VFIO cold-plug: NVIDIA"
```

---

## 4. Dev runtime class registration

`/etc/containerd/conf.d/kata-dev.toml`:

```toml
[plugins."io.containerd.cri.v1.runtime".containerd.runtimes.kata-qemu-nvidia-gpu-dev]
runtime_type = "io.containerd.kata-qemu-nvidia-gpu-dev.v2"
runtime_path = "/opt/kata-containers/target/aarch64-unknown-linux-musl/release/containerd-shim-kata-v2"
privileged_without_host_devices = true
pod_annotations       = ["io.katacontainers.*", "cdi.k8s.io/*"]
container_annotations = ["io.kubernetes.container.terminationMessage*"]

[plugins."io.containerd.cri.v1.runtime".containerd.runtimes.kata-qemu-nvidia-gpu-dev.options]
ConfigPath = "/etc/kata-containers/runtime-rs/configuration.toml"
```

Reload containerd after editing:

```bash
systemctl restart containerd
crictl info | grep -A2 kata-qemu-nvidia-gpu-dev
```

---

## 5. Dev shim configuration

`/etc/kata-containers/runtime-rs/configuration.toml` (key fields only):

```toml
[hypervisor.qemu]
path                = "/opt/kata/bin/qemu-system-aarch64"
kernel              = "/opt/kata/share/kata-containers/vmlinuz-nvidia-gpu.container"
image               = "/opt/kata/share/kata-containers/kata-containers-nvidia-gpu.img"
machine_type        = "virt"
machine_accelerators = "usb=off,gic-version=host,highmem-mmio-size=4T"
cold_plug_vfio      = "root-port"
default_memory      = 65536          # MiB; bump for multi-GPU
default_vcpus       = 16
```

Notes:
* `highmem-mmio-size=4T` is required to fit a single GB200's ~128 GiB coherent
  HBM BAR; multi-GPU may still fail due to per-root-port alignment.
* `cold_plug_vfio = "root-port"` is required so each VFIO device lands on its
  own PCIe root port (which the runtime-rs patch then tags with an
  `acpi-generic-initiator`).

---

## 6. CDI device map

`/etc/cdi/nvidia.yaml`:

```yaml
cdiVersion: "0.5.0"
kind: nvidia.com/vfio
devices:
  - name: gpu0
    containerEdits:
      deviceNodes:
        - path: /dev/vfio/<gpu0 iommu group>
  - name: gpu1
    containerEdits:
      deviceNodes:
        - path: /dev/vfio/<gpu1 iommu group>
  # ... gpu2, gpu3
```

Look up the IOMMU group ID per BDF:

```bash
for bdf in 0008:06:00.0 0009:06:00.0 0018:06:00.0 0019:06:00.0; do
  echo -n "$bdf -> group "
  readlink /sys/bus/pci/devices/$bdf/iommu_group | awk -F/ '{print $NF}'
done
```

---

## 7. Test pods

**Single-GPU pod** ([pod-cdi-1gpu.json](pod-cdi-1gpu.json)):

```json
{
  "metadata": {"name":"gpu-test-pod-1","namespace":"default","uid":"gpu-1gpu","attempt":1},
  "log_directory": "/tmp",
  "linux": {
    "security_context": { "privileged": true, "namespace_options": { "network": 2 } }
  },
  "annotations": {
    "io.katacontainers.config.hypervisor.cold_plug_vfio": "root-port",
    "io.katacontainers.config.runtime.vfio_mode": "vfio",
    "cdi.k8s.io/vfio": "nvidia.com/vfio=gpu1"
  }
}
```

Multi-GPU pod: [pod-cdi.json](pod-cdi.json) — set `cdi.k8s.io/vfio` to a
comma-separated list (e.g. `nvidia.com/vfio=gpu0,nvidia.com/vfio=gpu1,...`).

---

## 8. Run / re-run a pod

```bash
# Clean previous kata sandboxes (does NOT touch the libvirt reference VM)
crictl pods -q | xargs -r -I{} crictl rmp -f {} >/dev/null
pkill -9 -f containerd-shim-kata 2>/dev/null
for pid in $(pgrep -f qemu-system-aarch64); do
  grep -q "guest=gpu-vm" /proc/$pid/cmdline 2>/dev/null || kill -9 $pid
done
rm -rf /run/kata/* /run/kata-containers/shared/sandboxes/* /run/vc/* 2>/dev/null

# Launch
crictl --timeout 5m runp \
  --runtime kata-qemu-nvidia-gpu-dev \
  /opt/kata-containers/pod-cdi-1gpu.json

crictl pods
```

---

## 9. Inspecting what the shim actually runs

```bash
# Latest QEMU command line emitted by the dev shim:
for pid in $(pgrep -f qemu-system-aarch64); do
  grep -q "guest=gpu-vm" /proc/$pid/cmdline && continue   # skip libvirt
  tr '\0' ' ' < /proc/$pid/cmdline | tr ' ' '\n' | grep -E \
      "hmat=on|acpi-generic|vfio-pci|-numa node|vfio_gpu|highmem-mmio"
done

# Shim debug log
journalctl -u containerd -t kata --since "5 min ago" | tail -200
```

Expected lines for a 1-GPU pod after the GB200 patch:

```
-machine virt,...,hmat=on
-device vfio-pci,host=0009:06:00.0,...,id=vfio_gpu1
-numa node,nodeid=1
-object acpi-generic-initiator,id=gi0,pci-dev=vfio_gpu1,node=1
```

---

## 10. (Optional) Libvirt reference VM

A standalone libvirt VM (`/opt/vm-images/launch-gpu-vm.sh`) is provided as a
known-good reference for byte-comparing the guest SRAT/HMAT against what Kata
produces. The dev cleanup commands above intentionally skip QEMU processes
that contain `guest=gpu-vm` so the reference VM stays alive while iterating.

```bash
/opt/vm-images/launch-gpu-vm.sh 1gpu     # boot reference VM with 1 GPU
virsh list                                # confirm 'gpu-vm' running
virsh console gpu-vm                      # serial console
```

---

## 11. Troubleshooting cheat sheet

| Symptom | Likely cause | Action |
|---|---|---|
| `ttrpc: closed` on `crictl runp` | guest panic during boot | inspect `journalctl -u containerd` for QEMU stderr |
| `Failed to get NUMA node id for GPU memory` (NVRM) | Linux ARM64 doesn't propagate SRAT-GI to `pci_dev->numa_node` | known limitation — see PR description |
| `failed to assign requested address` for BARs | high-MMIO window too small for N GPUs | increase `highmem-mmio-size`, reduce GPU count |
| QMP handshake timeout | many cold-plugged VFIO devices | `DEFAULT_QMP_CONNECT_DEADLINE_MS` bump (already in this branch) |
| Pod stuck in `NotReady` | leftover sandbox state | run the cleanup block in §8 |

---

## 12. Iteration loop

1. Edit code under `src/runtime-rs/`.
2. `cargo build --release --target aarch64-unknown-linux-musl -p containerd-shim-kata-v2`
3. `crictl rmp -fa` (or the cleanup block in §8).
4. `crictl runp --runtime kata-qemu-nvidia-gpu-dev <pod>.json`
5. Inspect QEMU cmdline (§9) and `journalctl -u containerd`.

No `systemctl restart containerd` is needed between iterations — containerd
re-execs the shim binary at the path registered in `kata-dev.toml` on every
new sandbox.
