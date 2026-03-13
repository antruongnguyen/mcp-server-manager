use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar,
};
use objc2_foundation::{MainThreadMarker, NSData, NSString};
use tokio::sync::mpsc::UnboundedSender;

use crate::bridge::commands::AppCommand;

/// 18×18 PNG of the MCPSM logo (black on transparent).
static ICON_1X: &[u8] = include_bytes!("../../resources/statusbar.png");
/// 36×36 (2×) PNG of the MCPSM logo (black on transparent).
static ICON_2X: &[u8] = include_bytes!("../../resources/statusbar@2x.png");

/// Sender to the backend manager. Set from main.rs before run_status_bar.
static CMD_TX: OnceLock<UnboundedSender<AppCommand>> = OnceLock::new();

/// Dashboard port. Set from main.rs before run_status_bar.
static DASHBOARD_PORT: OnceLock<u16> = OnceLock::new();

define_class! {
    #[unsafe(super(objc2_foundation::NSObject))]
    #[name = "DashboardHelper"]
    #[ivars = ()]
    struct DashboardHelper;

    impl DashboardHelper {
        #[unsafe(method(openDashboard:))]
        fn _open_dashboard(&self, _sender: *mut AnyObject) {
            let port = DASHBOARD_PORT.get().copied().unwrap_or(3456);
            let _ = open::that(format!("http://127.0.0.1:{}", port));
        }

        #[unsafe(method(editConfig:))]
        fn _edit_config(&self, _sender: *mut AnyObject) {
            if let Ok(path) = crate::core::config::config_path() {
                let _ = open::that(path);
            }
        }

        #[unsafe(method(quit:))]
        fn _quit(&self, _sender: *mut AnyObject) {
            // Send Shutdown command so servers are stopped gracefully
            if let Some(tx) = CMD_TX.get() {
                let _ = tx.send(AppCommand::Shutdown);
            }
            // Give the backend a moment to stop servers, then terminate
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(500));
                std::process::exit(0);
            });
        }
    }
}

/// Provide the AppCommand sender so Quit can send Shutdown.
pub fn set_cmd_tx(tx: UnboundedSender<AppCommand>) {
    let _ = CMD_TX.set(tx);
}

/// Provide the dashboard port.
pub fn set_port(port: u16) {
    let _ = DASHBOARD_PORT.set(port);
}

/// Build an `NSImage` template image from the embedded status bar PNGs.
/// macOS will automatically handle light/dark mode tinting because it's
/// marked as a template image.
fn create_status_bar_icon() -> Option<Retained<NSImage>> {
    let data = NSData::with_bytes(ICON_1X);
    let img = NSImage::initWithData(NSImage::alloc(), &data)?;
    // Add the @2x representation so it's crisp on Retina displays.
    let data_2x = NSData::with_bytes(ICON_2X);
    if let Some(rep) = NSImage::initWithData(NSImage::alloc(), &data_2x)
        .and_then(|img2x| img2x.representations().firstObject())
    {
        img.addRepresentation(&rep);
    }
    // Mark as template so macOS handles light/dark mode tinting.
    img.setTemplate(true);
    Some(img)
}

/// Load an SF Symbol by name via `+[NSImage imageWithSystemSymbolName:accessibilityDescription:]`.
/// Returns `None` if the symbol isn't found (e.g. on older macOS versions).
fn sf_symbol(name: &str) -> Option<Retained<NSImage>> {
    let ns_name = NSString::from_str(name);
    let ptr: *mut NSImage = unsafe {
        msg_send![
            objc2::class!(NSImage),
            imageWithSystemSymbolName: &*ns_name,
            accessibilityDescription: std::ptr::null::<NSString>(),
        ]
    };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { Retained::retain(ptr) }.unwrap())
    }
}

/// Set up the macOS status bar icon with "Open Dashboard", "Edit Config", and "Quit" menu items,
/// then run the NSApplication event loop (blocks forever).
pub fn run_status_bar(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Create our helper that handles actions
    let helper: Retained<DashboardHelper> = unsafe {
        msg_send![DashboardHelper::alloc(), init]
    };

    // Create the status bar item
    let status_bar = NSStatusBar::systemStatusBar();
    let item = status_bar.statusItemWithLength(-1.0); // NSVariableStatusItemLength

    // Set the button image (template icon) or fall back to text
    if let Some(button) = item.button(mtm) {
        if let Some(icon) = create_status_bar_icon() {
            button.setImage(Some(&icon));
        } else {
            let title = NSString::from_str("MCP");
            button.setTitle(&title);
        }
    }

    // Build menu
    let menu = NSMenu::new(mtm);

    // Disabled title item at top
    let title_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("MCP Server Manager"),
            None,
            &NSString::from_str(""),
        )
    };
    menu.addItem(&title_item);

    let sep1 = NSMenuItem::separatorItem(mtm);
    menu.addItem(&sep1);

    // "Open Dashboard" item
    let open_title = NSString::from_str("Open Dashboard");
    let open_key = NSString::from_str("");
    let open_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &open_title,
            Some(sel!(openDashboard:)),
            &open_key,
        )
    };
    if let Some(img) = sf_symbol("gauge.with.dots.needle.bottom.50percent")
        .or_else(|| sf_symbol("gauge"))
    {
        open_item.setImage(Some(&img));
    }
    unsafe { open_item.setTarget(Some(&helper)) };
    menu.addItem(&open_item);

    // "Edit Config" item
    let edit_title = NSString::from_str("Edit Config");
    let edit_key = NSString::from_str("");
    let edit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &edit_title,
            Some(sel!(editConfig:)),
            &edit_key,
        )
    };
    if let Some(img) = sf_symbol("doc.badge.gearshape") {
        edit_item.setImage(Some(&img));
    }
    unsafe { edit_item.setTarget(Some(&helper)) };
    menu.addItem(&edit_item);

    let sep2 = NSMenuItem::separatorItem(mtm);
    menu.addItem(&sep2);

    // "Quit MCPSM" item — uses our quit: selector for graceful shutdown
    let quit_title = NSString::from_str("Quit MCPSM");
    let quit_key = NSString::from_str("q");
    let quit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &quit_title,
            Some(sel!(quit:)),
            &quit_key,
        )
    };
    if let Some(img) = sf_symbol("power") {
        quit_item.setImage(Some(&img));
    }
    unsafe { quit_item.setTarget(Some(&helper)) };
    menu.addItem(&quit_item);

    item.setMenu(Some(&menu));

    // Keep items alive for the lifetime of the app
    std::mem::forget(item);
    std::mem::forget(helper);

    app.run();
}
