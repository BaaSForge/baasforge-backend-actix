use actix_web::HttpResponse;

pub async fn handle_serve_chat_channel_ws_page() -> HttpResponse {
    let html = include_str!("../../chat-channel-ws.html");
    HttpResponse::Ok()
        .insert_header(("Content-Type", "text/html; charset=utf-8"))
        .body(html)
}
