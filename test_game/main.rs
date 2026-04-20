// d3d_test_game - Simulates a GPU-rendered game window for CI testing
// Renders animated colored rectangles using D3D11, mimicking real game behavior.
// Window title: "D3D Test Game" | Process: d3d_test_game.exe

use std::mem;
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{
            Direct3D::*,
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
};

fn main() -> Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;

        // Register window class
        let class_name = w!("D3DTestGameClass");
        let wc = WNDCLASSEXW {
            cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        // Create window — 1280x720 windowed, same as a typical game
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("D3D Test Game"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100, 100, 1280, 720,
            None, None,
            hinstance,
            None,
        )?;

        // Create D3D11 device + swap chain
        let swap_chain_desc = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: 1280,
                Height: 720,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                RefreshRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                ..Default::default()
            },
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            OutputWindow: hwnd,
            Windowed: TRUE,
            SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
            ..Default::default()
        };

        let mut device: Option<ID3D11Device> = None;
        let mut swap_chain: Option<IDXGISwapChain> = None;
        let mut context: Option<ID3D11DeviceContext> = None;

        D3D11CreateDeviceAndSwapChain(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            None,
            D3D11_CREATE_DEVICE_FLAGS::default(),
            None,
            D3D11_SDK_VERSION,
            Some(&swap_chain_desc),
            Some(&mut swap_chain),
            Some(&mut device),
            None,
            Some(&mut context),
        )?;

        let device = device.unwrap();
        let swap_chain = swap_chain.unwrap();
        let context = context.unwrap();

        // Get render target view from back buffer
        let back_buffer: ID3D11Texture2D = swap_chain.GetBuffer(0)?;
        let rtv = device.CreateRenderTargetView(&back_buffer, None)?;

        // Message + render loop
        // Colors cycle through to prove the capture is live, not a static frame
        let colors: [[f32; 4]; 6] = [
            [0.8, 0.2, 0.2, 1.0], // red
            [0.2, 0.8, 0.2, 1.0], // green
            [0.2, 0.2, 0.8, 1.0], // blue
            [0.8, 0.8, 0.2, 1.0], // yellow
            [0.8, 0.2, 0.8, 1.0], // magenta
            [0.2, 0.8, 0.8, 1.0], // cyan
        ];
        let mut frame: u64 = 0;

        let mut msg = MSG::default();
        loop {
            // Drain window messages
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return Ok(());
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            // Render — cycle color every 60 frames (~1 second at 60fps)
            let color_idx = ((frame / 60) % 6) as usize;
            let clear_color = colors[color_idx];

            context.ClearRenderTargetView(&rtv, &clear_color);
            context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);

            swap_chain.Present(1, 0).ok()?; // vsync on

            frame += 1;
        }
    }
}

extern "system" fn wnd_proc(
    hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}