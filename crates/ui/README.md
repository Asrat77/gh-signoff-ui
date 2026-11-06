# signoff-ui

Signoff's design system for [egui](https://github.com/emilk/egui): the palette,
type, icons, motion and widgets the popover and settings window are built from.
It draws the same on macOS, Windows and Linux.

## Using it

```rust
// Once, when the app starts:
signoff_ui::install(&ctx, signoff_ui::ThemeMode::System);

// In a frame:
use signoff_ui::widgets::{PrRow, Segment};
use signoff_ui::Status;

let checks = [Segment::Done, Segment::Done, Segment::Empty];
if ui
    .add(PrRow::new(Status::Attention, "Add partial signoff", "gh-signoff", 482).checks(&checks))
    .clicked()
{
    // open the pull request's page
}
```

Widgets read the current colours with `signoff_ui::palette(ctx)`, so they follow
the system's light or dark appearance. `theme::set_reduced_motion` turns
animation off.

## The look

- A black ground with raised 18 pt rows, and one ember accent that means "act
  here": the band across the top, failing pull requests, the step that needs you.
- Status colours are used only for state, and every state has its own glyph, so
  nothing depends on colour alone. Text meets WCAG AA contrast in both palettes;
  the tests check it.
- Inter (with a heavy display cut for headlines) and JetBrains Mono for branches,
  SHAs and command output.
- A pair of eyes is the mark. Their pupils look toward the trouble.

## Gallery

```sh
cargo run -p signoff-ui --example gallery            # dark
cargo run -p signoff-ui --example gallery -- --light  # light
cargo run -p signoff-ui --example gallery -- --run    # start the demo run
```

## Licences

JetBrains Mono is bundled under the SIL Open Font License 1.1
(`fonts/JetBrainsMono-LICENSE.txt`). Inter comes from `fastframe-fonts` under the
same licence, and the Phosphor icons from `egui-phosphor` under MIT.
