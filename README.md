# Coolscan

Simple scanning software for the **Nikon Super Coolscan 9000 ED** (LS-9000), built on [nkscan](https://github.com/activexray/nkscan). It runs on Linux and Windows.

Version 0.1 does the basics:
- connect to the scanner
- find the frames on the loaded film
- scan the selected frames
- save each one as a **16-bit linear TIFF with no adjustments**: no inversion, no curves, no colour profile

The background research behind it is in [RESEARCH.md](RESEARCH.md).

## Layout

| Crate | What it is |
|---|---|
| `crates/coolscan-core` | Scanner control, worker thread, TIFF writer and preview rendering. `NkscanBackend` drives real hardware; `FakeBackend` simulates a scanner. |
| `crates/coolscan-cli` | Command line tool, mainly for bringing up hardware |
| `crates/coolscan-gui` | Desktop app (egui) |

## Building

Rust is pinned by `rust-toolchain.toml` (1.97); `rustup` installs it on first build.

```sh
cargo build --release
cargo test
```

The binaries end up in `target/release/`: `coolscan-gui` and `coolscan-cli`.

### Linux
Install the GUI's system libraries (Debian/Ubuntu):
```sh
sudo apt install build-essential libxkbcommon-dev libgl1-mesa-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libwayland-dev libgtk-3-dev
```

### Windows
You need the MSVC toolchain: "Desktop development with C++" in Visual Studio, or the Visual Studio Build Tools with the C++ workload.

## Trying it without a scanner

```sh
cargo run -p coolscan-gui -- --demo
cargo run -p coolscan-cli -- --demo scan --out demo-scans
```

## Connecting the scanner

### Linux (recommended)
1. FireWire modules: `firewire_ohci`, `firewire_sbp2` and `sg`. Debian and Ubuntu load them automatically; check with `lsmod | grep -E 'firewire|sg'`.
2. The scanner shows up as `/dev/sgN`. `lsscsi -g` (package `lsscsi`) shows which one.
3. Give your user access without root via a udev rule, `/etc/udev/rules.d/60-coolscan.rules`:
   ```
   SUBSYSTEM=="scsi_generic", ATTRS{vendor}=="Nikon*", MODE="0660", GROUP="scanner"
   ```
   Then run `sudo groupadd -f scanner && sudo usermod -aG scanner $USER`, then `sudo udevadm control --reload && sudo udevadm trigger`, and log out and back in.
4. Run `coolscan-cli list`. It should print something like `/dev/sg3  Nikon LS-9000 ED`.

### Windows
- You need a FireWire card. The scanner must bind to Windows' scanner driver (`scsiscan.sys`) and appear under **Imaging devices** in Device Manager; the driver from VueScan or Nikon Scan does this.
- nkscan then finds it as `\\.\Scanner0`.
- Limitations of this path: no command timeouts, and a 128 KB transfer limit.

## Using the GUI
1. Pick the scanner and press **Connect**.
2. Choose film type, format, resolution, samples, output folder and file name.
   - *Film type* only changes how exposure is metered (colour negative meters each channel separately). It never changes the saved pixels.
   - *Format*: "Auto" asks the holder. A 120 holder can't tell 6×4.5 from 6×9, so choose the format there.
3. Insert the holder and press **Load / Preview**. The overview pass finds the frames, and you click frames to select or deselect them.
4. Press **Scan selected**. Files are saved as `<name>_<frame>.tif` and never overwrite existing files.
5. **Cancel** stops the current pass and ejects the film. **Eject** gives the film back.

"Invert preview" only changes the on-screen preview.

## Using the CLI

```sh
coolscan-cli list
coolscan-cli scan --dpi 2000 --samples 4 --film mono --format 67 --frames 1,3 --out ~/scans --name roll12
coolscan-cli eject
```

Add `--log debug` (or `--log nkscan=trace`) to see what nkscan is doing.

## Safety checks
Two kinds of check run before the stage moves the holder:
- **Format.** The app reads the holder ID and only allows the formats nkscan's holder table lists, refusing the rest before the overview pass. For example, the FH-869S takes 6×6, 6×7 or 6×9.
- **Frame position.** Before every frame, the app checks that the frame lies inside the holder's travel and is no longer than the scanner's limit, both as the scanner reports them. If not, it refuses instead of adjusting. nkscan has its own clamps on top of this.

In the GUI, changing the format after a preview disables Scan until you preview again.

The demo models an FH-869S. Its travel length is a made-up value, not a measured one.

## Status
- On the LS-9000, nkscan has only verified the FH-869S holder. Other holders are expected to work but haven't been tested.
- **Not tested on real hardware yet.** First tests should start at low resolution (`--dpi 1000`).

## Licence
GPL-3.0, see [LICENSE](LICENSE). nkscan itself is MIT OR Apache-2.0, which allows this. The TIFF writer is modelled on nkscan's CLI writer.
