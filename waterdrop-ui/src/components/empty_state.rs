use dioxus::prelude::*;

#[component]
pub fn EmptyState(icon: String, title: String, subtitle: String) -> Element {
    rsx! {
        div {
            class: "flex flex-col items-center justify-center py-12 text-center",
            span {
                class: "text-5xl mb-4 opacity-30",
                "{icon}"
            }
            h3 {
                class: "font-semibold text-base-content/40 text-lg",
                "{title}"
            }
            p {
                class: "text-sm text-base-content/30 mt-1",
                "{subtitle}"
            }
        }
    }
}
