use dioxus::prelude::*;

use crate::components::{DiscoverSection, SavedSection, SendDataModal, TopBar};
use crate::engine;
use crate::models::Device;

const FAVICON: Asset = asset!("/assets/favicon.ico");
const TAILWIND_CSS: Asset = asset!("/assets/tailwind.css");

#[component]
pub fn App() -> Element {
    // Global state
    let discovered_devices = use_signal(engine::fetch_discovered_devices);
    let saved_devices = use_signal(engine::fetch_saved_devices);
    let selected_device: Signal<Option<Device>> = use_signal(|| None);
    let is_scanning = use_signal(|| false);

    // Provide state via context so child components can access it
    use_context_provider(|| discovered_devices);
    use_context_provider(|| saved_devices);
    use_context_provider(|| selected_device);
    use_context_provider(|| is_scanning);

    rsx! {
        document::Link { rel: "icon", href: FAVICON }
        document::Stylesheet { href: TAILWIND_CSS }

        // Full-screen container with subtle gradient background
        div {
            class: "min-h-screen bg-base-200 text-base-content",
            "data-theme": "dark",

            // Top bar
            TopBar {}

            // Main content
            div {
                class: "max-w-5xl mx-auto px-4 sm:px-6 lg:px-8 pb-12",

                // Discover section
                DiscoverSection {}

                // Divider
                div { class: "divider divider-neutral my-2" }

                // Saved section
                SavedSection {}
            }

            // Send data modal (shown when a device is selected)
            SendDataModal {}
        }
    }
}
