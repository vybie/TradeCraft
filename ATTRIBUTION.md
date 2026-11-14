# Asset attribution

TradeCraft includes a modified copy of [egui-charts](https://github.com/userFRM/egui-charts), including its terminal. Its upstream license texts and third-party notices are in [vendor/egui-charts](vendor/egui-charts/). The upstream terminal manifest has no license field; TradeCraft adds one based on the upstream repository's license statement at the recorded revision, which does not specifically single out the terminal.

The experimental Pine interpreter is adapted from [DimmOFF's egui-charts fork](https://github.com/DimmOFF/egui-charts/tree/229207f089a8dd7ee89373dce1c9bc96309da7d4/examples/demo/pine_engine). The fork's [MIT license at that revision](https://github.com/DimmOFF/egui-charts/blob/229207f089a8dd7ee89373dce1c9bc96309da7d4/LICENSE-MIT) has the same copyright and terms as the vendored egui-charts MIT license. TradeCraft modifies its study adapter, diagnostics and execution safeguards.

TradeCraft uses the default fonts embedded by [epaint_default_fonts 0.37.0](https://crates.io/crates/epaint_default_fonts/0.37.0) through egui's `default_fonts` feature. No font binaries are maintained in this repository.

- **Hack Regular**: Copyright 2018 Source Foundry Authors, MIT; includes DejaVu public-domain work and Bitstream Vera, copyright 2003 Bitstream Inc. The full notices are in [Hack-Regular.txt](licenses/epaint-default-fonts/Hack-Regular.txt).
- **Ubuntu Light**: Ubuntu Font Licence 1.0. The full license and copyright notices are in [UFL.txt](licenses/epaint-default-fonts/UFL.txt).
- **egui-icons**: subset of emoji-icon-font, copyright 2014 John Slegers, MIT. The notice is in [emoji-icon-font-mit-license.txt](licenses/epaint-default-fonts/emoji-icon-font-mit-license.txt). Some glyphs represent third-party trademarks; attribution does not grant trademark rights.

These are asset notices, not a complete third-party dependency license audit. See [ARCHITECTURE.md](ARCHITECTURE.md) for dependency-license findings.
