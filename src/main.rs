use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::image::{BitsPerPixel, Image, ImageOrder, ScanlinePad};
use x11rb::protocol::render::{ConnectionExt as _, PictType};
use x11rb::protocol::shape::{self};
use x11rb::protocol::xfixes::{ConnectionExt as _, RegionWrapper};
use x11rb::protocol::xproto::{ConnectionExt as _, *};
use x11rb::reexports::x11rb_protocol::protocol::render;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt;
// use x11rb::xcb_ffi::XCBConnection;
use std::sync::Arc;
#[cfg(not(debug_assertions))]
use std::thread::{sleep, spawn};
#[cfg(not(debug_assertions))]
use std::time::Duration;

// use image::GenericImageView;
use image::ImageFormat;
use image::ImageReader;
#[cfg(not(debug_assertions))]
use rand::Rng;
use std::io::Cursor;

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};

x11rb::atom_manager! {
    pub Atoms: AtomCollectionCookie {
        WM_PROTOCOLS,
        _NET_WM_STATE,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_BELOW,
        _NET_WM_STATE_STICKY,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_WINDOW_OPACITY,
        GAMESCOPE_EXTERNAL_OVERLAY,
    }
}

/// Choose a visual to use. This function tries to find a depth=32 visual and falls back to the
/// screen's default visual.
fn choose_visual(
    conn: &impl Connection,
    screen: &Screen,
    screen_num: usize,
) -> Result<(u8, Visualid), ReplyError> {
    let depth = 32;

    // Try to use XRender to find a visual with alpha support
    let has_render = conn
        .extension_information(render::X11_EXTENSION_NAME)
        .unwrap()
        .is_some();
    if has_render {
        let formats = conn.render_query_pict_formats().unwrap().reply().unwrap();
        // Find the ARGB32 format that must be supported.
        let format = formats
            .formats
            .iter()
            .filter(|info| (info.type_, info.depth) == (PictType::DIRECT, depth))
            .filter(|info| {
                let d = info.direct;
                (d.red_mask, d.green_mask, d.blue_mask, d.alpha_mask) == (0xff, 0xff, 0xff, 0xff)
            })
            .find(|info| {
                let d = info.direct;
                (d.red_shift, d.green_shift, d.blue_shift, d.alpha_shift) == (16, 8, 0, 24)
            });
        if let Some(format) = format {
            // Now we need to find the visual that corresponds to this format
            if let Some(visual) = formats.screens[screen_num]
                .depths
                .iter()
                .flat_map(|d| &d.visuals)
                .find(|v| v.format == format.id)
            {
                return Ok((format.depth, visual.visual));
            }
        }
    }
    Ok((screen.root_depth, screen.root_visual))
}

fn composite_manager_running(
    conn: &impl Connection,
    screen_num: usize,
) -> Result<bool, ReplyError> {
    let atom = format!("_NET_WM_CM_S{}", screen_num);
    let atom = conn
        .intern_atom(false, atom.as_bytes())
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let owner = conn.get_selection_owner(atom).unwrap().reply()?;
    Ok(owner.owner != x11rb::NONE)
}

#[derive(Copy, Clone)]
struct Origin {
    x: i16,
    y: i16,
}

fn create_window(
    conn: Arc<RustConnection>,
    screen: &Screen,
    visual_id: Visualid,
    atoms: Atoms,
    window: u32,
    depth: u8,
) {
    let colormap =
        ColormapWrapper::create_colormap(&conn, ColormapAlloc::NONE, screen.root, visual_id)
            .unwrap();
    let win_aux = CreateWindowAux::new()
        .event_mask(EventMask::NO_EVENT)
        .background_pixel(x11rb::NONE)
        .border_pixel(x11rb::NONE)
        // important to be treated as "popup" window, which we want
        // https://tronche.com/gui/x/xlib/window/attributes/override-redirect.html
        .override_redirect(1)
        .colormap(colormap.colormap());

    conn.create_window(
        depth,
        window,
        screen.root,
        0,
        0,
        screen.width_in_pixels,
        screen.height_in_pixels,
        0,
        WindowClass::INPUT_OUTPUT,
        visual_id,
        &win_aux,
    )
    .unwrap();

    conn.change_property32(
        PropMode::REPLACE,
        window,
        atoms._NET_WM_STATE,
        AtomEnum::ATOM,
        &[
            atoms._NET_WM_STATE_FULLSCREEN,
            atoms._NET_WM_STATE_ABOVE,
            atoms._NET_WM_STATE_STICKY,
            atoms._NET_WM_STATE_SKIP_TASKBAR,
            atoms._NET_WM_STATE_SKIP_PAGER,
        ],
    )
    .unwrap();

    // set global window opacity. may not be needed
    conn.change_property32(
        PropMode::REPLACE,
        window,
        atoms._NET_WM_WINDOW_OPACITY,
        AtomEnum::CARDINAL,
        &[0x20cccccc],
    )
    .unwrap();

    // TODO:
    // https://github.com/Plagman/gamescope/issues/288
    // https://github.com/flightlessmango/MangoHud/blob/9a6809daca63cf6860ac9d92ae4b2dde36239b0e/src/app/main.cpp#L47
    // https://github.com/flightlessmango/MangoHud/blob/9a6809daca63cf6860ac9d92ae4b2dde36239b0e/src/app/main.cpp#L189
    // https://github.com/trigg/Discover/blob/de83063f3452b1cdee89b4c3779103eae2c90cbb/discover_overlay/overlay.py#L107
    conn.change_property32(
        PropMode::REPLACE,
        window,
        atoms.GAMESCOPE_EXTERNAL_OVERLAY,
        AtomEnum::CARDINAL,
        &[1],
    )
    .unwrap();

    conn.map_window(window).unwrap();
    conn.flush().unwrap();
}

fn create_region(conn: Arc<RustConnection>, window: u32, pixmap: Pixmap) {
    let region = RegionWrapper::create_region_from_bitmap(&conn, pixmap).unwrap();

    conn.xfixes_set_window_shape_region(window, shape::SK::INPUT, 0, 0, region.region())
        .unwrap();
    conn.flush().unwrap();
}

#[cfg(debug_assertions)]
const CHAR_HEIGHT: u16 = 170;
#[cfg(debug_assertions)]
const CHAR_WIDTH: u16 = 100;

#[cfg(debug_assertions)]
fn draw_letter(conn: Arc<RustConnection>, origin: Origin, letter: Vec<u8>, window: u32) {
    let reader = ImageReader::with_format(Cursor::new(letter), ImageFormat::Png);
    // sadly there is no 1-bit-png format in the image-crate
    let image = reader.decode().unwrap().into_luma8();

    assert!(image.width() as u16 == CHAR_WIDTH);
    assert!(image.height() as u16 == CHAR_HEIGHT);

    let mut img = Image::allocate(
        CHAR_WIDTH,
        CHAR_HEIGHT,
        ScanlinePad::Pad32,
        32,
        BitsPerPixel::B32,
        ImageOrder::MsbFirst,
    );
    for (x, y, pixel) in image.enumerate_pixels() {
        img.put_pixel(x as u16, y as u16, 0x88000000 | (pixel.0[0] as u32 * 255));
    }

    let gc = GcontextWrapper::create_gc(
        &conn,
        window,
        &CreateGCAux::new().graphics_exposures(0).foreground(1), //screen.white_pixel),
    )
    .unwrap();

    let pixmap = PixmapWrapper::create_pixmap(&conn, 32, window, CHAR_WIDTH, CHAR_HEIGHT).unwrap();
    img.put(&conn, pixmap.pixmap(), gc.gcontext(), 0, 0)
        .unwrap();

    // idk why I have to copy here, but I have to :/
    conn.copy_area(
        pixmap.pixmap(),
        window,
        gc.gcontext(),
        0,
        0,
        origin.x,
        origin.y,
        CHAR_WIDTH,
        CHAR_HEIGHT,
    )
    .unwrap();
    conn.flush().unwrap();
}

fn put_char(
    conn: Arc<RustConnection>,
    pos: Origin,
    encrypted_letter: &[u8],
    img: &mut Image,
    window: u32,
) {
    let key = Key::from_slice(&[
        145, 177, 108, 160, 218, 93, 51, 44, 185, 144, 149, 150, 190, 95, 105, 24, 240, 225, 25,
        86, 245, 86, 133, 241, 17, 209, 5, 196, 165, 236, 95, 88,
    ]);
    let cipher = ChaCha20Poly1305::new(key);

    let (read_nonce, encrypted_letter_plain) = encrypted_letter.split_at(12);

    let nonce = Nonce::from_slice(read_nonce);

    // letter has to consist of encrypted bytes + tag
    let letter = cipher
        .decrypt(nonce, encrypted_letter_plain.as_ref())
        .expect("Stop patching pls :(");

    let reader = ImageReader::with_format(Cursor::new(letter.clone()), ImageFormat::Png);
    // sadly there is no 1-bit-png format in the image-crate
    let image = reader.decode().unwrap().into_luma8();

    #[cfg(debug_assertions)]
    assert!(image.width() as u16 == CHAR_WIDTH);
    #[cfg(debug_assertions)]
    assert!(image.height() as u16 == CHAR_HEIGHT);

    for (x, y, pixel) in image.enumerate_pixels() {
        img.put_pixel(
            pos.x as u16 + x as u16,
            pos.y as u16 + y as u16,
            pixel.0[0] as u32,
        );
    }

    // debug drawing
    #[cfg(debug_assertions)]
    draw_letter(conn, pos, letter, window);
}

#[allow(non_snake_case)]
fn main() {
    let _letter_a = include_bytes!("../letters/a.png");
    let _letter_b = include_bytes!("../letters/b.png");
    let _letter_c = include_bytes!("../letters/c.png");
    let _letter_d = include_bytes!("../letters/d.png");
    let _letter_e = include_bytes!("../letters/e.png");
    let _letter_f = include_bytes!("../letters/f.png");
    let _letter_g = include_bytes!("../letters/g.png");
    let _letter_h = include_bytes!("../letters/h.png");
    let _letter_i = include_bytes!("../letters/i.png");
    let _letter_j = include_bytes!("../letters/j.png");
    let _letter_k = include_bytes!("../letters/k.png");
    let _letter_l = include_bytes!("../letters/l.png");
    let _letter_m = include_bytes!("../letters/m.png");
    let _letter_n = include_bytes!("../letters/n.png");
    let _letter_o = include_bytes!("../letters/o.png");
    let _letter_p = include_bytes!("../letters/p.png");
    let _letter_q = include_bytes!("../letters/q.png");
    let _letter_r = include_bytes!("../letters/r.png");
    let _letter_s = include_bytes!("../letters/s.png");
    let _letter_t = include_bytes!("../letters/t.png");
    let _letter_u = include_bytes!("../letters/u.png");
    let _letter_v = include_bytes!("../letters/v.png");
    let _letter_w = include_bytes!("../letters/w.png");
    let _letter_x = include_bytes!("../letters/x.png");
    let _letter_y = include_bytes!("../letters/y.png");
    let _letter_z = include_bytes!("../letters/z.png");
    let _letter_A = include_bytes!("../letters/A.png");
    let _letter_B = include_bytes!("../letters/B.png");
    let _letter_C = include_bytes!("../letters/C.png");
    let _letter_D = include_bytes!("../letters/D.png");
    let _letter_E = include_bytes!("../letters/E.png");
    let _letter_F = include_bytes!("../letters/F.png");
    let _letter_G = include_bytes!("../letters/G.png");
    let _letter_H = include_bytes!("../letters/H.png");
    let _letter_I = include_bytes!("../letters/I.png");
    let _letter_J = include_bytes!("../letters/J.png");
    let _letter_K = include_bytes!("../letters/K.png");
    let _letter_L = include_bytes!("../letters/L.png");
    let _letter_M = include_bytes!("../letters/M.png");
    let _letter_N = include_bytes!("../letters/N.png");
    let _letter_O = include_bytes!("../letters/O.png");
    let _letter_P = include_bytes!("../letters/P.png");
    let _letter_Q = include_bytes!("../letters/Q.png");
    let _letter_R = include_bytes!("../letters/R.png");
    let _letter_S = include_bytes!("../letters/S.png");
    let _letter_T = include_bytes!("../letters/T.png");
    let _letter_U = include_bytes!("../letters/U.png");
    let _letter_V = include_bytes!("../letters/V.png");
    let _letter_W = include_bytes!("../letters/W.png");
    let _letter_X = include_bytes!("../letters/X.png");
    let _letter_Y = include_bytes!("../letters/Y.png");
    let _letter_Z = include_bytes!("../letters/Z.png");
    let _letter_0 = include_bytes!("../letters/0.png");
    let _letter_1 = include_bytes!("../letters/1.png");
    let _letter_2 = include_bytes!("../letters/2.png");
    let _letter_3 = include_bytes!("../letters/3.png");
    let _letter_4 = include_bytes!("../letters/4.png");
    let _letter_5 = include_bytes!("../letters/5.png");
    let _letter_6 = include_bytes!("../letters/6.png");
    let _letter_7 = include_bytes!("../letters/7.png");
    let _letter_8 = include_bytes!("../letters/8.png");
    let _letter_9 = include_bytes!("../letters/9.png");
    let _letter__ = include_bytes!("../letters/_.png");
    let _letter_open = include_bytes!("../letters/{.png");
    let _letter_close = include_bytes!("../letters/}.png");
    let _letter_quote = include_bytes!("../letters/'.png");

    let _letters: Vec<&[u8]> = vec![
        _letter_a,
        _letter_b,
        _letter_c,
        _letter_d,
        _letter_e,
        _letter_f,
        _letter_g,
        _letter_h,
        _letter_i,
        _letter_j,
        _letter_k,
        _letter_l,
        _letter_m,
        _letter_n,
        _letter_o,
        _letter_p,
        _letter_q,
        _letter_r,
        _letter_s,
        _letter_t,
        _letter_u,
        _letter_v,
        _letter_w,
        _letter_x,
        _letter_y,
        _letter_z,
        _letter_A,
        _letter_B,
        _letter_C,
        _letter_D,
        _letter_E,
        _letter_F,
        _letter_G,
        _letter_H,
        _letter_I,
        _letter_J,
        _letter_K,
        _letter_L,
        _letter_M,
        _letter_N,
        _letter_O,
        _letter_P,
        _letter_Q,
        _letter_R,
        _letter_S,
        _letter_T,
        _letter_U,
        _letter_V,
        _letter_W,
        _letter_X,
        _letter_Y,
        _letter_Z,
        _letter_0,
        _letter_1,
        _letter_2,
        _letter_3,
        _letter_4,
        _letter_5,
        _letter_6,
        _letter_7,
        _letter_8,
        _letter_9,
        _letter__,
        _letter_open,
        _letter_close,
        _letter_quote,
    ];

    // get conn
    let (conn1, screen_num): (RustConnection, usize) = x11rb::connect(None).unwrap();
    let conn = Arc::new(conn1);

    // get screen
    let screen = &conn.setup().roots[screen_num];

    // min screen size
    if screen.width_in_pixels < 1900 || screen.height_in_pixels < 900 {
        println!("Screen too small :(, min size 1900 x 900");
        return;
    }

    // check if we support alpha channel
    let (depth, visual_id): (u8, Visualid) = choose_visual(&conn, screen, screen_num).unwrap();
    if depth < 32 {
        // Does not support alpha channel
        println!("Transparency not supported :(");
        return;
    }

    let compositor = composite_manager_running(&conn, screen_num).unwrap_or(false);
    if !compositor {
        println!("No composite manager running :(");
        return;
    }

    let atoms = Atoms::new(&conn).unwrap();
    let atoms = atoms.reply().unwrap();

    // enable xfixes (necessary for handling input regions)
    let _ = conn.xfixes_query_version(2, 0).unwrap();

    // main window id
    let win_id = conn.generate_id().unwrap();

    create_window(conn.clone(), screen, visual_id, atoms, win_id, depth);

    let pixmap = PixmapWrapper::create_pixmap(
        conn.clone(),
        1,
        win_id,
        screen.width_in_pixels,
        screen.height_in_pixels,
    )
    .unwrap();

    let gc = GcontextWrapper::create_gc(
        conn.clone(),
        pixmap.pixmap(),
        &CreateGCAux::new().graphics_exposures(0).foreground(0),
    )
    .unwrap();

    #[cfg(not(debug_assertions))]
    {
        let conn1 = conn.clone();
        let screen1 = screen.clone();
        spawn(move || loop {
            sleep(Duration::from_millis(100));
            let tree_reply = conn1.query_tree(screen1.root).unwrap().reply().unwrap();
            for child in tree_reply.children {
                if child == win_id {
                    continue;
                }

                conn1
                    .change_property32(
                        PropMode::REPLACE,
                        child,
                        atoms._NET_WM_STATE,
                        AtomEnum::ATOM,
                        &[atoms._NET_WM_STATE_BELOW],
                    )
                    .unwrap();

                let values = ConfigureWindowAux::default()
                    .x(rand::thread_rng().gen_range(0..screen1.width_in_pixels) as i32)
                    .y(rand::thread_rng().gen_range(0..screen1.height_in_pixels) as i32)
                    .width(rand::thread_rng().gen_range(100..screen1.width_in_pixels) as u32)
                    .height(rand::thread_rng().gen_range(100..screen1.height_in_pixels) as u32);
                conn1.configure_window(child, &values).unwrap();
            }
            conn1.flush().unwrap();
        });
    }

    let mut img = Image::allocate(
        screen.width_in_pixels,
        screen.height_in_pixels,
        ScanlinePad::Pad8,
        1,
        BitsPerPixel::B1,
        ImageOrder::MsbFirst,
    );

    put_char(
        conn.clone(),
        Origin { x: 400, y: 100 },
        _letter_open,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 200, y: 90 },
        _letter_x,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 500, y: 110 },
        _letter_A,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 700, y: 100 },
        _letter_w,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1100, y: 110 },
        _letter__,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 900, y: 100 },
        _letter_y,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 100, y: 110 },
        _letter_h,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1200, y: 140 },
        _letter_h,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 600, y: 90 },
        _letter_l,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1500, y: 120 },
        _letter__,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 750, y: 410 },
        _letter_o,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1000, y: 130 },
        _letter_s,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 550, y: 420 },
        _letter_n,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1300, y: 90 },
        _letter_a,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1600, y: 700 },
        _letter_n,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 800, y: 90 },
        _letter_4,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 450, y: 410 },
        _letter_e,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 650, y: 440 },
        _letter__,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 250, y: 400 },
        _letter_b,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1150, y: 430 },
        _letter_0,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 300, y: 120 },
        _letter_p,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 950, y: 420 },
        _letter__,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 850, y: 400 },
        _letter_N,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 350, y: 430 },
        _letter_3,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1500, y: 720 },
        _letter_3,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1250, y: 440 },
        _letter_u,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1050, y: 410 },
        _letter_y,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1100, y: 700 },
        _letter_S,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1350, y: 400 },
        _letter_r,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1450, y: 410 },
        _letter__,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1200, y: 710 },
        _letter_c,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1700, y: 710 },
        _letter_close,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1400, y: 700 },
        _letter_E,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1400, y: 100 },
        _letter_5,
        &mut img,
        win_id,
    );
    put_char(
        conn.clone(),
        Origin { x: 1300, y: 730 },
        _letter_r,
        &mut img,
        win_id,
    );
    img.put(&conn, pixmap.pixmap(), gc.gcontext(), 0, 0)
        .unwrap();

    create_region(conn.clone(), win_id, pixmap.pixmap());

    loop {
        conn.wait_for_event().unwrap();
    }
}
