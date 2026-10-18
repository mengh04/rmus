//! `org.mpris.MediaPlayer2`：应用身份与退出。

use super::Mpris;
use mpris_server::RootInterface;
use zbus::fdo;

impl RootInterface for Mpris {
    async fn raise(&self) -> fdo::Result<()> {
        // rmus 无 GUI 可 raise
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        let _ = std::fs::remove_file(rmus::socket_path());
        std::process::exit(0);
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn set_fullscreen(&self, _: bool) -> zbus::Result<()> {
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok("rmus".into())
    }

    async fn identity(&self) -> fdo::Result<String> {
        Ok("rmus".into())
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(vec!["file".into()])
    }
}
