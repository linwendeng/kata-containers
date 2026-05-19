# Kata-Containers GB200 GPU Passthrough — Status Report

**Date:** 2026-05-19
**Runtime:** `kata-qemu-nvidia-gpu-dev` (runtime-rs, aarch64)
**Goal:** Drive a pod with 4× NVIDIA GB200 + 4× Mellanox CX VFIO passthrough to `SANDBOX_READY`.

---

## TL;DR

Original driver/firmware mismatch is **fixed**. Guest now boots, mounts rootfs, loads the
NVIDIA 580.126.20 driver, and the GSP firmware loads on all four GB200s.

A **new architectural blocker** has surfaced that prevents `RmInitAdapter` from
completing: kata runtime-rs does not emit the QEMU NUMA + ACPI Generic Initiator
topology required by the `nvgrace_gpu_vfio_pci` host driver for Grace-Blackwell C2C
coherent memory passthrough.

---

## What was found

### 1. Original symptom (resolved)
- Host driver 580.126.20, guest image had 595.58.03 driver/firmware.
- Xid 120 GSP panic, WPR2 stuck on subsequent attempts.

### 2. Build-pipeline issues uncovered while rebuilding (all resolved)
| # | Issue | Fix |
|---|---|---|
| 1 | Docker daemon missing | `apt install moby-engine` (29.4.3) |
| 2 | Build container couldn't reach `ports.ubuntu.com` from this Azure host | Dockerfile sed swap to `azure.ports.ubuntu.com` + `docker run --add-host=ports.ubuntu.com:91.189.92.21` |
| 3 | git "dubious ownership" in build container | `git config --system --add safe.directory '*'` |
| 4 | apt resolver pulled 595.x driver deps | Pin `libnvidia-*-580` + `nvidia-firmware-580=580.126.20-1ubuntu1` in `nvidia_chroot.sh` |
| 5 | `libnvidia-nscq-580` not in CUDA repo | Keep `libnvidia-nscq` unversioned |
| 6 | Guest panic `VFS Unable to mount root fs ext4` | `rootfs_type = "erofs"` in `configuration.toml` |
| 7 | Guest panic `erofs blkszbits 14 isn't supported` (mkfs.erofs defaults to host page size 64K, guest has 4K pages) | `mkfs.erofs -b 4096` in `image_builder.sh` |
| 8 | UPX `NotCompressibleException` on `libstdbuf.so` killed build | `\|\| echo "skipped"` in `nvidia_rootfs.sh` |
| 9 | Firmware mismatch (driver 580 looking for `/lib/firmware/nvidia/580.126.20/gsp_*.bin`, rootfs only had 595.71.05) | Versioned `nvidia-firmware-580` install |

### 3. New blocker (open)

After the rebuild succeeded, the guest now reaches NVIDIA driver init successfully:

```
NVRM: loading NVIDIA UNIX Open Kernel Module for aarch64 580.126.20
nvidia 0000:03:00.0: enabling device (0100 -> 0102)   ← all 4 GPUs detected
```

GSP firmware now loads from the correct path. But init fails with:

```
NVRM: kmemsysSetupCoherentCpuLink_IMPL: Failed to get NUMA node id for GPU memory
                                                      @ kern_mem_sys.c:207
NVRM: kbifCacheVFInfo_GB100: Unable to read NV_PF0_INITIAL_AND_TOTAL_VFS
NVRM: RmInitNvDevice: *** Cannot initialize the device
NVRM: GPU 0000:0X:00.0: RmInitAdapter failed! (0x24:0x40:1207)
... (retry) ...
NVRM: _kgspBootGspRm: unexpected WPR2 already up, cannot proceed with booting GSP
NVRM: GPU 0000:0X:00.0: RmInitAdapter failed! (0x62:0x40:2015)
panic: panicked at src/execute.rs:24:9:
/bin/nvidia-ctk failed with status: exit status: 1
```

#### Root cause
Host uses `nvgrace_gpu_vfio_pci` — the Grace-Blackwell variant driver that exposes
GPU HBM memory via the NVLink-C2C coherent region rather than a normal PCI BAR.
The guest GB100/Blackwell driver requires that this memory be presented as a
**dedicated NUMA node** with an **ACPI Generic Initiator** that links it back to
the vfio-pci device. This is the standard upstream model for GB200 VM passthrough.

Required QEMU constructs (per GPU):
```
-numa node,nodeid=<N>,initiator=<N>
-object acpi-generic-initiator,id=gi<N>,pci-dev=hostdev<N>,node=<N>
-machine virt,hmat=on,...
```

Plus possibly a `memory-backend-ram` per node or an HMAT/SRAT attribute.

#### What runtime-rs emits today
`src/runtime-rs/crates/hypervisor/src/qemu/cmdline_generator.rs:1160` has a
generic `NumaNode` struct, but it is used only for hugepage memory backing — not
wired to VFIO devices. `VfioDeviceModern` (`device/driver/vfio.rs`,
`device/driver/vfio_device/*`) has no `numa_node_id` field, and the QEMU cmdline
generator never emits `acpi-generic-initiator`.

---

## What was done this session

### Code changes (kept)
- `versions.yaml`: pin `externals.nvidia.driver.version: "580.126.20"`.
- `tools/packaging/kata-deploy/local-build/dockerbuild/Dockerfile`: apt sources sed
  swap (azure mirror) + `git config --system --add safe.directory '*'`.
- `tools/packaging/kata-deploy/local-build/kata-deploy-binaries-in-docker.sh`:
  `docker run --add-host=ports.ubuntu.com:91.189.92.21`.
- `tools/osbuilder/rootfs-builder/nvidia/nvidia_chroot.sh`:
  - Install `libnvidia-{cfg1,gl,extra,decode,fbc1,encode,compute}-${driver_major}`
    + `libnvidia-nscq` (unversioned).
  - Install/hold `nvidia-firmware-${driver_major}=${driver_version}-1ubuntu1`.
- `tools/osbuilder/rootfs-builder/nvidia/nvidia_rootfs.sh`: skip UPX failures.
- `tools/osbuilder/image-builder/image_builder.sh:600`:
  `mkfs.erofs -b 4096 -zlz4hc -Enoinline_data ...`.
- `/etc/kata-containers/runtime-rs/configuration.toml`:
  - `rootfs_type = "erofs"`.
  - Updated `kernel_verity_params` to the new root_hash.

Pre-existing fixes from earlier work (not touched, verified intact):
`vfio.rs::filter_bridge_device`, `vfio_device/core.rs::filter_bridge_device`,
`vfio_device/device.rs::VfioDeviceModern.extra_bus_port_ids`,
`qemu/cmdline_generator.rs::VfioDeviceConfig.iommufd_id`, `qemu/inner.rs`
`VfioModern` arm, `qemu/qmp.rs::DEFAULT_QMP_CONNECT_DEADLINE_MS = 600000`.

### Artifacts installed
- `/opt/kata/share/kata-containers/kata-ubuntu-noble-nvidia-gpu-580.126.20.image`
  (455 MB, 4K erofs, 580.126.20 driver + firmware verified).
- `kata-containers-nvidia-gpu.img` symlink updated.
- `root_hash_nvidia-gpu.txt` =
  `b2e4b36f59cff1ad1aa24b5b981f5cd87245cbf6c58e55845eadb97d6676f7ca`.

### Verifications performed
- `mount -t erofs` of new image succeeds; `/lib/firmware/nvidia/580.126.20/gsp_ga10x.bin` present.
- Guest boot: all 4 GB200s `enabling device (0100 -> 0102)`.
- Guest dmesg: driver loads `NVIDIA UNIX Open Kernel Module for aarch64 580.126.20`.
- GSP firmware load: **succeeds** (previous `error -2` gone).
- VFIO topology emitted by kata: 8 root ports, 4 iommufd objects, 4 GPUs + 4 NICs correctly attached.

---

## What to do next

Pick one of the following paths.

### Option A — Implement GB200 NUMA-aware passthrough in runtime-rs *(weeks of work)*
1. Add `numa_node_id: Option<u32>` to `VfioDeviceModern` (and propagate from
   device manager — possibly auto-allocated per GPU device with vendor 0x10de
   and host driver `nvgrace_gpu_vfio_pci`).
2. In `qemu/cmdline_generator.rs`:
   - Emit `-machine virt,hmat=on` when any device requires it.
   - For each GB200 device, emit `-numa node,nodeid=N,initiator=N` and
     `-object acpi-generic-initiator,id=giN,pci-dev=hostdevN,node=N`.
   - Verify ordering: numa nodes must come *before* the `acpi-generic-initiator`
     that references them, which must come *after* the `vfio-pci` `id=hostdevN`.
3. Test QMP `query-numa` from inside guest to verify nodes are exposed.
4. Update integration tests / NVIDIA stack docs in `docs/`.

### Option B — Quick experiment: drop `nvgrace_gpu_vfio_pci`, try plain `vfio-pci` *(hours)*
Rebind the 4 GB200s from `nvgrace_gpu_vfio_pci` to `vfio-pci` and re-run the pod.
Likely fails (the C2C memory will not be exposed and the driver will report a
different error), but it cleanly answers whether the C2C path is strictly
required. Cheap to validate.

```
echo 0000:08:06.0 > /sys/bus/pci/devices/0008:06:00.0/driver/unbind
echo "10de 2941" > /sys/bus/pci/drivers/vfio-pci/new_id
# repeat for 0009/0018/0019 06:00.0
```

### Option C — Defer GPU init to first reach `SANDBOX_READY` *(quick proof-of-life)*
Blacklist `nvidia` in the guest so the kernel boots, agent comes up, sandbox
becomes ready, then attempt `modprobe nvidia` manually from inside the pod (and
deal with init from there). Useful only if the user's success criterion is
literally `SANDBOX_READY` regardless of GPU usability inside the pod.

```
# in tools/osbuilder/rootfs-builder/nvidia: add /etc/modprobe.d/nvidia-blacklist.conf
echo "blacklist nvidia" > .../etc/modprobe.d/nvidia-blacklist.conf
```

### Option D — Investigate `nvidia-ctk` panic in detail
Even with the driver fully working, the pod creation eventually hits
`nvidia-ctk failed with status: exit status: 1` in the guest. The CDI hook may
need extra config to tolerate partial GPU init. Worth a look once Option A/B
unblocks the driver init.

---

## Reference paths
- Build log: `/tmp/kata-build.log`
- Session notes: `/memories/session/kata-gb200-build.md`
- Pod spec: `/opt/kata-containers/pod-cdi.json`
- Runtime config: `/etc/kata-containers/runtime-rs/configuration.toml` (backup `.bak-<ts>` next to it)
- Last guest console log: `journalctl -t kata --since "30 min ago"`
