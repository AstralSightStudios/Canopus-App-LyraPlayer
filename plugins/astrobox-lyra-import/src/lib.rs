use crate::astrobox::psys_host_v4::canopusconnect::Payload;
use crate::exports::astrobox::psys_plugin_v4::{
    canopusconnect as plugin_connect, event, event::EventType, lifecycle,
};

mod artwork;
mod connect;
mod id3;
mod import;
mod library;
mod lyrics;
mod netease;
mod sniff;
mod state;
mod ui;
mod view;

wit_bindgen::generate!({
    path: "wit",
    world: "psys-world-v4-canopusconnect",
    generate_all,
});

struct LyraImportPlugin;

impl event::Guest for LyraImportPlugin {
    async fn on_event(event_type: EventType, event_payload: String) -> String {
        match event_type {
            EventType::DeviceAction => {
                connect::refresh_devices().await;
                ui::rerender();
            }
            EventType::Timer => {
                if event_payload == import::STALL_TIMER_PAYLOAD {
                    import::check_stall();
                    ui::rerender();
                }
            }
            EventType::InterconnectMessage
            | EventType::PluginMessage
            | EventType::ProviderAction
            | EventType::DeeplinkAction
            | EventType::TransportPacket => {}
        }
        String::new()
    }

    async fn on_ui_event(event_id: String, _event: event::UiEvent, event_payload: String) -> String {
        ui::on_event(&event_id, &event_payload).await;
        String::new()
    }

    async fn on_ui_render(element_id: String) {
        ui::render_main_ui(&element_id);
    }

    async fn on_card_render(_card_id: String) {}
}

impl plugin_connect::Guest for LyraImportPlugin {
    async fn on_message(device_addr: String, module: String, payload: Payload) {
        if let Some(frame) = connect::binary_frame(&module, payload) {
            if !import::on_frame(&device_addr, &frame).await {
                library::on_frame(&device_addr, &frame).await;
            }
            ui::rerender();
        }
    }
}

impl lifecycle::Guest for LyraImportPlugin {
    async fn on_load() {
        tracing_subscriber::fmt()
            .with_writer(std::io::stdout)
            .with_ansi(false)
            .compact()
            .init();
        connect::refresh_devices().await;
        let restored = state::load_netease_session();
        state::with_state(|state| {
            state.netease_audio_bitrate =
                netease::normalized_audio_bitrate(state.netease_audio_bitrate);
            if let Ok(Some(cookie)) = &restored {
                state.netease_cookie = cookie.clone();
            }
            // The page itself shows the login and device state; only a problem needs words.
            if let Err(error) = &restored {
                state.status = error.clone();
            }
        });
    }
}

export!(LyraImportPlugin);
