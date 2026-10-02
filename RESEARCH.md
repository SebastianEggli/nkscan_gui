# Nikon Super Coolscan 9000 ED: software research

**Goal:** control an LS-9000 over FireWire from a Linux machine with self-written (Rust) software. The main reason is to escape Nikon Scan's handling of negatives. Black & white scans in particular come out very contrasty, with blown areas that can't be recovered.

Research date: 2026-10-02.

---

## 1. How the scanner talks

- The LS-9000 is **FireWire (IEEE 1394) only**; it has no USB. It uses **SBP-2**, the same protocol as FireWire disks, which carries **SCSI commands**.
- On Linux, the kernel modules `firewire-ohci` + `firewire-core` + `firewire-sbp2` + `sg` turn it into a generic SCSI device (`/dev/sgN`). **No custom kernel driver is needed.** All scanner control is done from user space by sending SCSI command blocks via the `SG_IO` ioctl.
- **Nikon's official interface spec is available:** [LS9kIFSpec.md](https://github.com/activexray/nkscan/blob/main/docs/LS9kIFSpec.md) (with [LS5kIFSpec](https://github.com/activexray/nkscan/blob/main/docs/LS5kIFSpec.md) for the LS-5000, described as protocol-identical). The original .docx files were contributed by @kosma. Contents:
  - Standard SCSI commands: TEST UNIT READY, INQUIRY, MODE SELECT/SENSE, RESERVE/RELEASE, SET/GET WINDOW (24h/25h), SCAN (1Bh), READ (28h), SEND (2Ah).
  - Nikon vendor commands: ABORT C0h, EXECUTE C1h, SET PARAMETER E0h, GET PARAMETER E1h.
  - Exposure time **per colour channel**, in 10 ns units (1 … 0x03FFFFFF).
  - Output is **16 bits per channel only**.
  - Resolution is 666–4000 dpi (X).
  - Focus range is 0–450.
  - Film holder detection uses the INQUIRY VPD page C1h. There are 15 holder types: FH-835M/S, FH-869S/G/GR/M, FH-816, FH-8G1, …
- **Key point for this project:** the spec's "host cooperation" flags state that *thumbnail creation, multisample averaging and truncation are done by the driver*. The scanner returns linear sensor data, and **inversion, contrast and tone curves are never done by the hardware**. Whatever Nikon Scan does to the contrast is a software decision, so it can be replaced completely.

### Platform notes
- **Linux:** Debian and Ubuntu ship the FireWire modules. RHEL 9 hardening guides [blacklist `firewire-core`](https://stigviewer.com/stig/red_hat_enterprise_linux_9/2024-02-19/finding/V-257806), so avoid the RHEL family or remove that blacklist.
- **Long-term risk:** the Linux FireWire maintainer (Takashi Sakamoto) [plans to push users off IEEE 1394 from 2026 and close the subsystem in 2029](https://www.phoronix.com/news/Linux-Firewire-New-Maintainer). He also gives `firewire-sbp2` low priority. Mitigation: pin an LTS kernel or distro on the scanning box.
- **Windows 11** still works through a PCIe FireWire card and the inbox 1394/SBP-2 stack plus `scsiscan.sys`. nkscan's Windows transport uses `IOCTL_SCSISCAN_CMD`, which has no timeouts and a 128 KB transfer limit. This is a viable backup path.
- **macOS** dropped FireWire in Tahoe. ASFireWire restores it on Apple Silicon, and nkscan is tested with it.

---

## 2. What already exists

| Project | What it is | Relevance for LS-9000 / this goal |
|---|---|---|
| **[nkscan](https://github.com/activexray/nkscan)** (Rust, MIT/Apache-2.0) | Full driver library + CLI + Python bindings, written from Nikon's spec. About 470 commits and actively developed in 2026. | **Best foundation by far.** Works over FireWire on Linux and Windows. Confirmed on a real LS-9000. No GUI and no inversion. Details below. |
| [Nikon-Coolscan-RE](https://github.com/kevihiiin/Nikon-Coolscan-RE) | Reverse engineering of the scanner firmware (H8/3003) and the NikonScan 4.0.3 DLLs, with a protocol knowledge base in `docs/kb/` | Reference for undocumented behaviour, and for **what Nikon Scan actually does internally**. Focus is the LS-50 (USB). |
| [coolscanpy](https://github.com/rohanpandula/coolscanpy/) / [ScanStudio](https://github.com/rohanpandula/ScanStudio) | Python USB driver + macOS GUI (GPL-3.0 bridge) | USB only. The LS-9000 is "discovery only, unsupported for scanning". Useful as a GUI and workflow reference. |
| [SANE coolscan3](https://www.mankier.com/5/sane-coolscan3) / coolscan2 | C backends for LS-30/40/50/2000/4000/8000 | **No LS-9000.** A [2008 patch attempt](https://lists.alioth.debian.org/pipermail/sane-devel/2008-April/021794.html) was never merged. Historical reference only. |
| [openICE](https://github.com/a6o/openICE) (C#/C, GPL-3.0), [digital-fauxice](https://github.com/rohanpandula/digital-fauxice) | Open reimplementations of Digital ICE (dust removal using the infrared channel) | Post-processing stage for nkscan's IR output. |
| NegPy (marcinz606/NegPy) | Negative inversion and colour processing | Already used downstream of nkscan by an LS-9000 user ([nkscan #57](https://github.com/activexray/nkscan/issues/57)). Reference for inversion. |
| [coolscan-mods](https://github.com/kosma/coolscan-mods) | Firmware mods for the LS-40/LS-50 | Not relevant to the LS-9000, but its author supplied the Nikon specs. |
| [VueScan](https://www.hamrick.com/vuescan/nikon_ls_9000.html), SilverFast, Nikon Scan 4.0.3 | Closed source | Baselines for comparison. VueScan can save raw linear files too. |

---

## 3. nkscan in depth

### Architecture
```
src/transport/   linux.rs (SG_IO on /dev/sg*, 512 KB reserved buffer, real timeouts)
                 windows.rs (scsiscan.sys), darwin.rs, usb.rs
src/protocol/    cdbs, window, decode, curves (CCD row calibration), sense, caps, model
src/session/     probe, focus, autoexpose, scan, image, window
src/scan/        meter, autoexpose, pass, frame/framing/strip, focus, clean, profile, thumbnail
src/bin/nkscan/  CLI: cli, scan, mono, io, dump, eject
profiles/        Nikon ICC profiles (derived from Nikon's specs)
```

- The Linux transport already handles the quirk where `firewire-sbp2` repacks SBP-2 status into synthetic sense data. Nikon's vendor field ends up in sense bytes 15–17, and this is confirmed on an LS-9000.
- **LS-9000 holder support:**
  - FH-869S: ✅ verified.
  - FH-835M, 835S, 869G, 869GR, 869M, 816, 8G1: ⚠️ theoretically supported.
  - An open issue with the FH-835S is [#57](https://github.com/activexray/nkscan/issues/57).

### The image data path, step by step

1. **Metering pass** ([scan/meter.rs](https://github.com/activexray/nkscan/blob/main/src/scan/meter.rs)):
   - Takes a quick pass and finds the **99.9th-percentile brightest sample** per channel. Using a percentile means a dust speck can't set the exposure.
   - Scales exposure linearly so that value lands at **97% of full scale**, leaving 3% headroom.
   - A channel already at full scale gets its exposure halved and is re-metered, up to 3 passes.
   - Colour negative meters each channel separately, which cancels the orange mask in the exposure itself. Slide, Kodachrome and **B&W keep the white balance locked**, moving all channels by the same factor.
   - **What this means for a negative:** the brightest samples are the *thinnest* film, i.e. film base and the scene's shadows. They are **protected from clipping by design**. The densest parts (the scene's highlights) end up lowest, near the sensor's noise floor.
2. **Exposure** ([scan/autoexpose.rs](https://github.com/activexray/nkscan/blob/main/src/scan/autoexpose.rs)): per-channel exposure times in 10 ns units. Metering is always done on the host.
3. **Read and decode** (`protocol/decode.rs`, `scan/pass.rs`): line-interleaved 16-bit data.
   - [protocol/curves.rs](https://github.com/activexray/nkscan/blob/main/src/protocol/curves.rs) applies **CCD row correction** in multi-line mode. It uses calibration curves the scanner measures about itself, with piecewise-linear interpolation.
   - This only evens out differences between CCD rows. It is **not a tone curve**.
4. **Multisampling** (`--samples N`): the host averages N reads, cutting noise by about √N. It works directly against the noise floor at the dense end.
5. **Infrared** (`--ir`, `--clean`): IR is saved as a separate `_IR.tiff`, and optional dust cleaning uses it.
6. **Mono** ([bin/nkscan/mono.rs](https://github.com/activexray/nkscan/blob/main/src/bin/nkscan/mono.rs)): converts RGB to luminance using the Y row of Nikon's mono profile matrix after the profile's tone-reproduction curves, then clamps to 0–1 and stores 16-bit. **There is no inversion and no auto-contrast.**
7. **Output** ([bin/nkscan/io.rs](https://github.com/activexray/nkscan/blob/main/src/bin/nkscan/io.rs)):
   - **16-bit linear TIFF, still a negative**, with the Nikon ICC profile embedded where one exists.
   - In nkscan's own words: *"the samples are linear"*.

**Conclusion:** nkscan already produces exactly the clean starting point this project needs. It gives a linear, uninverted, 16-bit capture with the thin end protected from clipping, and no tone decisions baked in.

### Automatic frame detection

**Yes, nkscan finds frames automatically**, much like Nikon Scan's strip thumbnails. You call `nkscan scan` without `--frames`, or `session.discover_frames()` in Python. You get back a list of frame rectangles plus the overview thumbnail, and `--frames 1,3,5` then picks individual frames.

#### Step 1: choose a framing mechanism ([scan/framing.rs](https://github.com/activexray/nkscan/blob/main/src/scan/framing.rs))

The choice depends on what the scanner and holder report about themselves (`Framing::choose()`):

| Mechanism | When it's used | How frames are found |
|---|---|---|
| **Published** | The unit already reports measured frame rectangles. This applies to masked holders that "know their own geometry", such as slide-mount holders. | The scanner's frame table is taken as-is. |
| **Thumbnail** | The unit uses frame-rectangle coordinates and supports a thumbnail pass. This is the normal case for strip holders. | Host-side image analysis of an overview scan (steps 2–3 below). |
| **Perforation** | The unit can read perforation data. This applies to 35 mm on units that count sprocket holes. | The perforation table is read, and frame boundaries are written back to the scanner (`DataType::Perforation` → `Boundary2`). |
| **Address** | Fallback | The holder's fixed geometry from the INQUIRY address page. |

The code doesn't say which LS-9000 holders end up in which branch. Strip holders like the FH-869S and FH-835S almost certainly use **Thumbnail**. Running `nkscan dump` on the real scanner would confirm it.

#### Step 2: the thumbnail (overview) pass ([scan/thumbnail.rs](https://github.com/activexray/nkscan/blob/main/src/scan/thumbnail.rs))
- One pass covers the whole film axis at the scanner's lowest resolution, with line-ordered colour channels. The window starts at the very beginning of the holder axis, *"so the leading edge of the film is in the pass and can be found"*.
- **The host builds the thumbnail, not the scanner.** The spec's host-cooperation bits say *"the unit hands us the pass and expects us to make sense of it"*. Nikon Scan has to do the same work on the computer side.
- Each thumbnail column is one line pitch of film:
  - **Computed** pitch comes from optical dpi divided by thumbnail resolution.
  - **Measured** pitch is used on units with perforation tables (35 mm). nkscan falls back to the computed pitch if the measurement looks implausible. It uses the ISO 1007 perforation spacing of 4.7498 mm.

#### Step 3: finding gaps and pitch on the strip ([scan/strip.rs](https://github.com/activexray/nkscan/blob/main/src/scan/strip.rs))

The key idea, from the source: *"The film between two frames holds no picture, so it reads the same all the way down the sensor. A picture does not."*

1. **Per-column signals.** For every column along the film:
   - **Detail** is the standard deviation down the sensor axis, averaged over colour planes. It is normalised so the busiest 90% of columns sit at about 1.
   - **Level** is the mean brightness, normalised between the 2nd and 98th percentiles.
   - The 8 rows at each sensor edge are dropped (`TRIM = 8`) to ignore holder edges.
2. **Find the film.** Columns whose detail is near the flattest 10% (`BARE = 0.10`, `TOLERANCE = 0.15`) count as bare film. Flat stretches shorter than 70% of a frame inside a picture are merged, since a flat sky is not a gap (`CLOSE = 0.70`). Picture runs shorter than 50% of a frame are discarded as holder edges, mask or backlight (`RUN = 0.50`).
3. **Fit a regular grid.** The search covers pitch candidates of 18–31 twentieths of the format length (`PITCH`, so 0.9–1.55 × frame length) and every possible first-gap offset. Each candidate gets a score:
   ```
   score = pictures/frames − gaps/(frames+1) − 2.0 · disagreement
   ```
   High detail inside frames scores well, and flat gaps score well. The gaps should also all read the same, since they are all unexposed film (`AGREEMENT = 2.0`). The best fit's score is the reported **`contrast`**.
4. **Frame rectangles.** Frame k spans from `first + k·pitch + REACH + 1` to `first + (k+1)·pitch − REACH`, with `REACH = 2` columns of margin around each gap. These are then mapped back to scanner addresses.

Useful properties:
- **Polarity-independent.** Only gaps are compared with each other, so negative, slide and B&W are handled identically.
- **Blank frames.** An unexposed or black frame is still counted, because *"the fit spans it"*: its neighbours fix its position.
- **Format-aware.** The film format comes from the holder, or from `--format 135|half|645|66|67|68|69|<mm>`, and sets the expected frame length.
- **No fit.** If no candidate fits, discovery returns no frames.

#### Known weak spots (open nkscan issues)
- "Scan stalls on strips that start with blank or black empty film"
- "Frame detection with underexposed frame" (LS-5000)
- "Wrong size with custom length"
- Feature requests: "Individual frame offset option" (manual nudge per frame, like Nikon Scan's frame offset) and "Thumbnail Review" (look at and correct the detected frames before scanning)
- [#57](https://github.com/activexray/nkscan/issues/57): fast batch preview of a whole holder, like Nikon Scan's

**What this means for our project:** the detection algorithm is solid and well documented, but there's **no way to review or correct it interactively**. A GUI for this project could show the thumbnail strip with the detected frames as draggable boxes, with per-frame offset and size. Corrections would go back through the library API. That fills exactly the gaps listed above and is where Nikon Scan's workflow is still more convenient.

### What nkscan does *not* have (the gaps)
- **No inversion or positive tone pipeline.** This is deliberately left to downstream tools.
- **No CLI flags for manual exposure** or for the metering percentile and target. The library and Python API do allow it: `session.meter_frame(...)` returns exposures, and `session.scan_frame(frame, exposures=..., lock_white_balance=..., ...)` accepts them. See [PYTHON.md](https://github.com/activexray/nkscan/blob/main/PYTHON.md).
- **No exposure bracketing (HDR).** This is open issue [#35 "Extend dynamic range with bracketed exposure + linear merge"](https://github.com/activexray/nkscan/issues/35):
  - Idea: take a short exposure that protects the thin end and a long one that lifts the dense end above noise. Merge them per pixel: use the longest exposure that isn't clipped, divided by the exposure ratio. The result stays linear.
  - The maintainer wants measurements on dense negatives before deciding, and is unsure whether it should be the default since it doubles scan time.
- **No GUI.** The author is open to contributions but won't build one.

---

## 4. Why Nikon Scan's B&W scans come out too contrasty (hypotheses)

There are probably **two separate causes**, and only one of them is software:

### A. Software: Nikon Scan's internal inversion and tone mapping
- Nikon Scan inverts the negative and applies its own processing: auto-levels/auto-contrast with clip percentages, plus its tone curve, often in an 8-bit or clamped chain. You never get to see the raw data.
- Anything pushed past black or white is clipped **before** you get the file. That matches "blown out parts that are not recoverable".
- There are also [reports of a Nikon Scan glitch](https://www.photrio.com/forum/threads/nikonscan-workflow.191503/) where scans turn high-contrast and clip after B&W scanning, plus a [rangefinderforum thread](https://rangefinderforum.com/threads/impossible-to-scan-b-w-without-clipping.120521/).
- The same photrio thread notes that on the 9000, exposing for the film base lets you set the black point without clipping.

### B. Physics: the Callier effect
- The Coolscan uses **collimated LED light**. Silver-grain B&W negatives scatter collimated light, so dense areas (the scene's highlights) behave **denser** than under a diffuse light source. A condenser enlarger shows the same effect.
- The negative's effective density range on this scanner really is larger, and grain and scratches stand out more. Chromogenic B&W films such as XP2 or BW400CN are dye-based and barely show the effect.
- **Software can't remove this, but it can compensate.** A gentler inversion curve brings the larger range back down, *as long as the raw capture holds usable signal in the densest areas*.

### The fix in one line
**Linear raw capture → controlled inversion.** Protect the thin end (nkscan already does). Lift the dense end above noise with multisampling or bracketing. Then invert in software with an explicit, user-controlled curve that rolls off softly instead of clipping.

---

## 5. Creating DNG files

### Why DNG
- DNG is a TIFF-based container for **linear sensor data**.
- Raw editors treat a DNG as unprocessed data, with full 16-bit or float headroom. That is exactly what controlled inversion needs. The editors that can invert negatives from it:
  - darktable's **negadoctor** module
  - RawTherapee's **Film Negative** tool
  - Lightroom/ACR with Negative Lab Pro
- nkscan's TIFF is linear too, but nothing in the file *says* so. Editors then assume display gamma and the tones come out wrong.
- **There is precedent:**
  - [VueScan already writes scanner DNGs](https://www.hamrick.com/vuescan/html/vuesc20.htm): LinearRaw, 4 × 16-bit samples (RGB + IR), no gamma.
  - [openICE](https://github.com/a6o/openICE) takes these RGBI DNGs (from VueScan or SilverFast) as input and is tuned for the LS-5000/9000.
  - [A published B&W workflow](https://staff.washington.edu/shrike/photography/photography-workflow-processing-black-and-white-negatives-with-vuescan-and-darktable/) inverts negatives from VueScan DNGs with darktable's negadoctor.
- The DNG becomes the archival **"digital negative"**. Your own inverter, darktable or RawTherapee can then work from it non-destructively, and you can redo the inversion years later.

### File layout: a minimal valid DNG for a scanner

A DNG is a TIFF with extra tags.
- Cameras usually put a preview in IFD0 (the TIFF's first image directory) and the raw image in a SubIFD (a nested directory).
- **The raw image may also sit directly in IFD0.** That's valid and keeps writing simple.
- An 8-bit preview is optional and can go in a second IFD with `NewSubFileType = 1`.

| Tag | Value for the LS-9000 |
|---|---|
| NewSubFileType | 0 (main raw image) |
| DNGVersion / DNGBackwardVersion | 1.4.0.0 / 1.1.0.0, or 1.4.0.0 when storing float data |
| Make / Model / UniqueCameraModel | "Nikon" / "LS-9000 ED" / "Nikon LS-9000 ED" |
| PhotometricInterpretation | **34892 = LinearRaw**: every pixel already has full RGB, so no demosaicing. This is the same as camera DNGs after demosaicing. |
| SamplesPerPixel | 3 (RGB), **1 (B&W mono, ¹⁄₃ of the file size)**, or 4 (RGBI, the VueScan convention) |
| BitsPerSample / SampleFormat | 16 / 1 (unsigned int). With the bracketed merge (HDR) from [#35](https://github.com/activexray/nkscan/issues/35): 16/24/32-bit **float, SampleFormat 3**. Float support arrived in [DNG 1.4](https://www.photographyblog.com/news/adobe_releases_version_1.4_of_dng_specification) to hold more than 16 bits of range. |
| Compression | 1 (none) or 8 (Deflate, lossless) |
| BlackLevel / WhiteLevel | sensor dark offset (0 to start; to be measured) / 65535 |
| [ColorMatrix1](https://awaresystems.be/imaging/tiff/tifftags/colormatrix1.html) + CalibrationIlluminant1 | Matrix from XYZ to scanner RGB. It can be derived from the primaries in Nikon's ICC profiles in nkscan's `profiles/`. Per nkscan, *"the device primaries are identical across the film types of one model"*, so one matrix fits all film types. **Not required for mono files.** |
| AsShotNeutral | For colour negatives, the film-base RGB, so the raw editor neutralises the orange mask. Otherwise 1, 1, 1. |
| XResolution / YResolution | scan dpi (e.g. 4000), so print sizes come out right |
| Orientation | mirror and rotation as flags, rather than moving pixels |
| DefaultCropOrigin / DefaultCropSize | the detected frame, when the scan was taken slightly larger than the frame |
| XMP or DNGPrivateData | per-channel exposure times (10 ns units), focus position, holder, film type, frame number, sample count, nkscan version. This makes every scan reproducible. |

### Writing it, step by step
1. Get the planes from nkscan (`ScanResult.colors`, linear u16 arrays) and the scan metadata (exposures, focus, dpi, holder).
2. Optionally merge the multisampled or bracketed passes. A bracketed merge produces float data.
3. Interleave the planes into RGB (or keep a single plane for mono) and write strips or tiles, optionally Deflate-compressed.
4. Write the tags from the table above. Compute `ColorMatrix1` once from the Nikon profile and store it as a constant.
5. Optionally add a small gamma-encoded, inverted 8-bit preview IFD, so file browsers show a positive thumbnail.
6. Validate the file (see below).

### Infrared channel options
- **(a)** Store IR as a 4th sample, like VueScan. Works directly with openICE, but support in general raw editors still needs testing.
- **(b)** Keep IR in a separate file, as nkscan does today with `_IR.tiff`.
- **(c)** Run IR dust cleaning first and write a clean RGB DNG. This is what openICE outputs.
- **Recommendation:** an RGB or mono master DNG plus a sidecar IR file. Write RGBI only when feeding openICE.

### Rust options
- **The `tiff` crate** (image-rs) is the simplest path.
  - nkscan already uses it for its TIFF output, and it can write arbitrary tags (nkscan already does `write_tag(Tag::IccProfile, …)`).
  - With the raw image in IFD0, no SubIFDs are needed.
- **The [`dng` crate](https://docs.rs/crate/dng/latest)** ([apertus dng-rs](https://github.com/apertus-open-source-cinema/dng-rs), v1.6.0, July 2026) is an option for experimenting and debugging.
  - It's a low-level DNG reader/writer from the AXIOM open cinema camera project.
  - Its `dump_dng` / `compile_dng` tools turn a DNG into editable YAML and back. That's handy for picking apart a VueScan reference DNG and copying its tag layout.
- **Validation:**
  - Adobe DNG SDK's `dng_validate`, the reference checker.
  - `exiftool -a -G1 file.dng`, to inspect the tags.
  - Opening the file in darktable, RawTherapee and Lightroom.

### Where DNG fits in the pipeline
```
nkscan capture (multisample / bracketing)
   → DNG master  (linear negative + all scan metadata)          ← archive this
   → inversion   (own tool, or darktable negadoctor / RawTherapee Film Negative)
   → positive TIFF / JPEG
```

### Open points to check on hardware
- Do darktable, RawTherapee and Lightroom accept 1-sample (mono) and 4-sample (RGBI) LinearRaw files from a "camera" they don't know? 3-sample RGB is the safe default.
- Does the scanner have a dark offset that should go into BlackLevel? This could be measured with the light source blocked or from a very dense area.
- Scan one frame with VueScan (raw DNG) and with nkscan → DNG, then compare tags and values with `dump_dng`.

---

## 6. Proposed project direction (not started)

A Rust tool, later possibly a GUI, with **nkscan as a library dependency**:

1. **Capture:**
   - nkscan raw linear 16-bit with locked white balance and optional multisampling.
   - Optional 2-exposure bracketing with linear merge. Implement it as a contribution to nkscan issue #35.
   - Always keep the raw negative as a **DNG master** (see section 5).
2. **Measure:** find the film base level (Dmin) from the rebate or unexposed area, and the densest useful level (Dmax). Convert to density: `D = −log10(T / T_base)`.
3. **Invert and tone-map:**
   - Use a "paper grade"-style characteristic curve in density space, with adjustable contrast (grade), toe and shoulder.
   - Show a histogram with clipping warnings. Never clip silently.
4. **Export:** a 16-bit positive TIFF next to the DNG master. IR dust cleaning (openICE-style) is optional.

Possible later GUI work: preview, frame selection and crop, per-frame focus and exposure, and the batch preview of a whole holder requested in nkscan #57.

---

## 7. First hands-on experiment

This tests the hypotheses before writing any code.

1. **Linux box setup:**
   ```sh
   lsmod | grep -E 'firewire|sg'      # firewire_ohci, firewire_sbp2, sg loaded?
   dmesg | grep -i sbp2               # scanner logged in as SBP-2 target?
   lsscsi -g                          # shows type "scanner" + /dev/sgN
   sg_inq /dev/sgN                    # vendor "Nikon", product LS-9000
   cargo install nkscan --features cli --locked
   ```
2. **Pick one "problem" B&W negative** that Nikon Scan blows out, then scan it:
   ```sh
   nkscan scan --film Mono --samples 1 --basename test_s1
   nkscan scan --film Mono --samples 8 --basename test_s8
   ```
3. **Inspect the raw linear TIFF histograms** with a quick numpy script:
   - Is the thin end around 97% and not clipped?
   - How far above the noise floor (the sample-to-sample noise in dark areas) is the dense end?
   - How much does `--samples 8` lower that noise?
4. **Invert by hand** in darktable or GIMP (32-bit mode) with a gentle curve, and compare with Nikon Scan's output.
   - **If the highlights hold:** the hypothesis is confirmed, and the fix is purely an inversion and tone pipeline.
   - **If the dense end is buried in noise:** bracketing (#35) is the key feature to build, and measuring this helps the nkscan maintainer too.

---

## Sources
- nkscan: [repo](https://github.com/activexray/nkscan), [docs.rs](https://docs.rs/crate/nkscan/latest), [PYTHON.md](https://github.com/activexray/nkscan/blob/main/PYTHON.md), [LS9kIFSpec](https://github.com/activexray/nkscan/blob/main/docs/LS9kIFSpec.md), [issue #35](https://github.com/activexray/nkscan/issues/35), [issue #57](https://github.com/activexray/nkscan/issues/57)
- [Nikon-Coolscan-RE](https://github.com/kevihiiin/Nikon-Coolscan-RE), [coolscan-mods](https://github.com/kosma/coolscan-mods), [coolscanpy](https://github.com/rohanpandula/coolscanpy/), [ScanStudio](https://github.com/rohanpandula/ScanStudio), [openICE](https://github.com/a6o/openICE)
- SANE: [sane-coolscan3 man page](https://www.mankier.com/5/sane-coolscan3), [2008 LS-9000 mailing list post](https://lists.alioth.debian.org/pipermail/sane-devel/2008-April/021794.html)
- [Phoronix: Linux FireWire maintained until 2029](https://www.phoronix.com/news/Linux-Firewire-New-Maintainer)
- [VueScan LS-9000 page](https://www.hamrick.com/vuescan/nikon_ls_9000.html)
- DNG: [VueScan "How VueScan Works" (raw DNG output)](https://www.hamrick.com/vuescan/html/vuesc20.htm), [B&W VueScan + darktable negadoctor workflow](https://staff.washington.edu/shrike/photography/photography-workflow-processing-black-and-white-negatives-with-vuescan-and-darktable/), [dng crate](https://docs.rs/crate/dng/latest) / [dng-rs](https://github.com/apertus-open-source-cinema/dng-rs), [ColorMatrix1 tag](https://awaresystems.be/imaging/tiff/tifftags/colormatrix1.html), [DNG 1.4 (floating point)](https://www.photographyblog.com/news/adobe_releases_version_1.4_of_dng_specification)
- B&W clipping and Callier: [photrio Nikon Scan workflow thread](https://www.photrio.com/forum/threads/nikonscan-workflow.191503/), [rangefinderforum: impossible to scan B&W without clipping](https://rangefinderforum.com/threads/impossible-to-scan-b-w-without-clipping.120521/), [rangefinderforum: Coolscan Callier effect](https://rangefinderforum.com/goto/post?id=2081413)
