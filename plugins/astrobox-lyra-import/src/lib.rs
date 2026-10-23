use wit_bindgen::FutureReader;

use crate::astrobox::psys_host_v4::canopusconnect::Payload;
use crate::exports::astrobox::psys_plugin::{event_v3 as event, event_v3::EventType, lifecycle};
use crate::exports::astrobox::psys_plugin_v4::canopusconnect as plugin_connect;

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

wit_bindgen::generate!({
    path: "wit",
    world: "psys-world-v4-canopusconnect",
    generate_all,
});

struct LyraImportPlugin;

impl event::Guest for LyraImportPlugin {
    fn on_event(event_type: EventType, event_payload: _rt::String) -> FutureReader<String> {
        match event_type {
            EventType::DeviceAction => {
                connect::refresh_devices();
                ui::rerender();
            }
            EventType::Timer => {
                if event_payload == import::STALL_TIMER_PAYLOAD {
                    wit_bindgen::block_on(import::check_stall());
                    ui::rerender();
                }
            }
            EventType::InterconnectMessage
            | EventType::PluginMessage
            | EventType::ProviderAction
            | EventType::DeeplinkAction
            | EventType::TransportPacket => {}
        }
        immediate_string(String::new())
    }

    fn on_ui_event_v3(
        event_id: _rt::String,
        _event: event::Event,
        event_payload: _rt::String,
    ) -> FutureReader<_rt::String> {
        ui::on_event(&event_id, &event_payload);
        immediate_string(String::new())
    }

    fn on_ui_render(element_id: _rt::String) -> FutureReader<()> {
        ui::render_main_ui(&element_id);
        immediate_unit()
    }

    fn on_card_render(_card_id: _rt::String) -> FutureReader<()> {
        immediate_unit()
    }
}

impl plugin_connect::Guest for LyraImportPlugin {
    fn on_message(
        device_addr: _rt::String,
        module_id: _rt::String,
        payload: Payload,
    ) -> FutureReader<()> {
        if let Some(frame) = connect::binary_frame(&module_id, payload) {
            wit_bindgen::block_on(async {
                if !import::on_frame(&device_addr, &frame).await {
                    library::on_frame(&device_addr, &frame).await;
                }
            });
            ui::rerender();
        }
        immediate_unit()
    }
}

impl lifecycle::Guest for LyraImportPlugin {
    fn on_load() {
        tracing_subscriber::fmt()
            .with_writer(std::io::stdout)
            .with_ansi(false)
            .compact()
            .init();
        connect::refresh_devices();
        let restored = state::load_netease_session();
        state::with_state(|state| {
            state.netease_audio_bitrate =
                netease::normalized_audio_bitrate(state.netease_audio_bitrate);
            if let Ok(Some(cookie)) = &restored {
                state.netease_cookie = cookie.clone();
            }
            state.status = match &restored {
                Err(error) => error.clone(),
                Ok(Some(_)) => "已恢复网易云登录状态。".to_string(),
                Ok(None) if state.devices.is_empty() => "未发现已连接设备。".to_string(),
                Ok(None) => "请确认手表上已启用 Lyra Player，然后选择本地或网易云音乐。".to_string(),
            };
        });
    }
}

fn immediate_string(value: String) -> FutureReader<String> {
    let (writer, reader) = wit_future::new(String::new);
    wit_bindgen::spawn(async move {
        let _ = writer.write(value).await;
    });
    reader
}

fn immediate_unit() -> FutureReader<()> {
    let (writer, reader) = wit_future::new::<()>(|| ());
    wit_bindgen::spawn(async move {
        let _ = writer.write(()).await;
    });
    reader
}

export!(LyraImportPlugin);
