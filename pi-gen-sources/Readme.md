## pi-gen-sources

The old pi-gen image build was retired. The SD card image is now built by
[`image/build-image.sh`](../image/build-image.sh); see
[BUILD.md](../BUILD.md#sd-card-image).

Two files stay here because installers and the updater of already-released
versions fetch them by this path:

- `00-dashusb-tweaks/files/dashusb-pick-binary`: picks the per-CPU
  binary at each service start (`install-pi.sh`, `crates/sentryusb/src/migrate.rs`)
- `00-dashusb-tweaks/files/dashusb.conf.sample`: the documented sample
  config (`install-pi.sh`)
