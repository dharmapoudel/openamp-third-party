# Openamp third-party component sources

Source packages for the named MPL-2.0 and LGPL-2.1 components used by Openamp. Openamp's own code is not here and is proprietary.

Tag `v2.5.3` records this source collection from Openamp skin-flex source `5a9c1a051b7474a74350d8c56ea0a8b98d30d3ba`. It describes that source baseline, not a claim that every APK labeled 2.5.3 has these exact dependencies. jaudiotagger remains here for APKs shipped before its replacement is verified and delivered.

| Component | Version | License | Upstream | Modified |
|---|---|---|---|---|
| Symphonia (nine crates) | 0.5.5 | MPL-2.0 | https://github.com/pdeljanov/Symphonia | Yes; `symphonia/PATCHES.md` lists the changes |
| libusb | 1.0.30 | LGPL-2.1-or-later | https://github.com/libusb/libusb | No |
| libprojectM | 4.1.8 | LGPL-2.1-or-later; bundled vendor licenses retained | https://github.com/projectM-visualizer/projectm | No |
| jaudiotagger | 3.0.1 | LGPL-2.1-or-later | https://bitbucket.org/ijabz/jaudiotagger | No |

## Provenance

- `symphonia/`: byte-identical to all 181 files under Openamp `core/third_party/symphonia/` at the source baseline above, including LICENSE and PATCHES.md. Upstream base commit `6d533f26150953a882a6a111ebd13f0abf7129d5` is recorded in PATCHES.md.
- `libusb/`: extracted from the exact release archive used by the build, https://github.com/libusb/libusb/releases/download/v1.0.30/libusb-1.0.30.tar.bz2 . SHA256 `fea36f34f9156400209595e300840767ab1a385ede1dc7ee893015aea9c6dbaf`. Upstream tag `v1.0.30`, commit `87a55632db62c9bdc58cd31d3ccfa673f1bb017f`. COPYING retained.
- `libprojectM/`: extracted from the exact release archive used by the build, https://github.com/projectM-visualizer/projectm/releases/download/v4.1.8/libprojectM-4.1.8.tar.gz . SHA256 `f72135e8b8e5fd807f908537255b41ac7ba4830309265e830cb756535487239e`. Upstream tag `v4.1.8`, commit `4967dc003e5206d7df3ff447287f7609c47d2343`. COPYING, LICENSE.txt and vendor license files retained.
- `jaudiotagger/`: exact source-JAR contents from https://repo.maven.apache.org/maven2/net/jthink/jaudiotagger/3.0.1/jaudiotagger-3.0.1-sources.jar plus the LGPL-2.1 license text. The source JAR does not include that license text, but its Java headers identify LGPL-2.1-or-later.

This repository contains source and its upstream supporting files only. It contains no Openamp app code, keys, compiled libraries, APKs or build outputs. This collection does not replace the applicable license terms or assert completion of every distribution obligation.
