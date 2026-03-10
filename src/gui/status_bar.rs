use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuItem, NSStatusBar,
};
use objc2_foundation::{MainThreadMarker, NSString};

define_class! {
    #[unsafe(super(objc2_foundation::NSObject))]
    #[name = "DashboardHelper"]
    #[ivars = ()]
    struct DashboardHelper;

    impl DashboardHelper {
        #[unsafe(method(openDashboard:))]
        fn _open_dashboard(&self, _sender: *mut AnyObject) {
            let _ = open::that("http://127.0.0.1:17532");
        }
    }
}

/// Set up the macOS status bar icon with "Open Dashboard" and "Quit" menu items,
/// then run the NSApplication event loop (blocks forever).
pub fn run_status_bar(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Create our helper that handles the openDashboard: action
    let helper: Retained<DashboardHelper> = unsafe {
        msg_send![DashboardHelper::alloc(), init]
    };

    // Create the status bar item
    let status_bar = NSStatusBar::systemStatusBar();
    let item = status_bar.statusItemWithLength(-1.0); // NSVariableStatusItemLength

    // Set the button title
    if let Some(button) = item.button(mtm) {
        let title = NSString::from_str("MCP");
        button.setTitle(&title);
    }

    // Build menu
    let menu = NSMenu::new(mtm);

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
    unsafe { open_item.setTarget(Some(&helper)) };
    menu.addItem(&open_item);

    let sep = NSMenuItem::separatorItem(mtm);
    menu.addItem(&sep);

    let quit_title = NSString::from_str("Quit MCPSM");
    let quit_key = NSString::from_str("q");
    let quit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &quit_title,
            Some(sel!(terminate:)),
            &quit_key,
        )
    };
    menu.addItem(&quit_item);

    item.setMenu(Some(&menu));

    // Keep items alive for the lifetime of the app
    std::mem::forget(item);
    std::mem::forget(helper);

    app.run();
}
