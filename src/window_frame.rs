use crate::sidebar::{ink, ui_font};
use crate::workspace::Workspace;
use rune::{Cx, El, ImageCache, col, el, row, text};

pub const HEIGHT: f32 = 53.;

pub fn titlebar(w: &Workspace, cx: &mut Cx<Workspace>) -> El {
    let insets = cx.chrome_insets();
    cx.caption_color(ink(0xd5dce5));
    let brand = w.caption_brand.get_or_init(|| {
        ImageCache::new(w.wake.clone())
            .load(&include_bytes!("../assets/brand/png/mcli-caption.png")[..])
    });
    let mut contents = row()
        .w_full()
        .grow(1.)
        .min_h(0.)
        .pl(insets.leading + 23.)
        .pr(insets.trailing)
        .items_center()
        .clip_soft(0.)
        .child(
            el().key("caption-brand")
                .role("img")
                .label("Mcli")
                .size(96., 32.)
                .pixels(brand.pixels()),
        );
    if w.layout.viewport.get()[0] - insets.leading - insets.trailing >= 325. {
        contents = contents
            .child(el().w(19.))
            .child(
                el().key("caption-divider")
                    .size(1., 19.)
                    .fill(ink(0x474e5a)),
            )
            .child(el().w(18.))
            .child(
                text(if crate::single_instance::is_dev_channel() {
                    "多 Agent 终端工作台 · DEV"
                } else {
                    "多 Agent 终端工作台"
                })
                .key("caption-description")
                .font(ui_font(16., 460.))
                .color(ink(0xd2d7df)),
            );
    }
    col()
        .key("workspace-titlebar")
        .w_full()
        .h(HEIGHT)
        .window_drag()
        .linear_gradient(
            std::f32::consts::FRAC_PI_2,
            [
                (0., ink(0x0d111b)),
                (0.40, ink(0x101420)),
                (1., ink(0x141d2f)),
            ],
        )
        .child(contents)
        .child(el().w_full().h(1.).fill(ink(0x2b323e)))
}
