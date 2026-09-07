//! D-Bus interface definition (introspection XML) shared by daemon and GUI.
//!
//! - History entries travel as `(ssssx)` tuples: (file, source, title, copyright or empty, applied timestamp)
//! - Gallery entries travel as `(sssx)` tuples: (file, title, source, downloaded timestamp)

pub const BUS_NAME: &str = "github.nwkyz.Equinox.Daemon1";
pub const OBJECT_PATH: &str = "/github/nwkyz/Equinox/Daemon1";
pub const INTERFACE: &str = "github.nwkyz.Equinox.Daemon1";

pub const INTROSPECTION_XML: &str = r#"<node>
  <interface name="github.nwkyz.Equinox.Daemon1">
    <method name="GetStatus">
      <arg type="s" name="wallpaper_source" direction="out"/>
      <arg type="s" name="wallpaper_mode" direction="out"/>
      <arg type="s" name="current_file" direction="out"/>
    </method>
    <method name="GetLastError">
      <arg type="s" name="message" direction="out"/>
    </method>
    <method name="FetchNow">
      <arg type="s" name="source" direction="in"/>
    </method>
    <method name="GetSourceImages">
      <arg type="s" name="source" direction="in"/>
      <arg type="a(sssx)" name="images" direction="out"/>
    </method>
    <method name="SetWallpaperSource">
      <arg type="s" name="source" direction="in"/>
      <arg type="s" name="mode" direction="in"/>
    </method>
    <method name="ApplyWallpaper"/>
    <method name="ApplyLatest">
      <arg type="s" name="source" direction="in"/>
    </method>
    <method name="ApplyRandom">
      <arg type="s" name="source" direction="in"/>
    </method>
    <method name="ApplyFile">
      <arg type="s" name="path" direction="in"/>
    </method>
    <method name="Next"/>
    <method name="Previous"/>
    <method name="GetHistory">
      <arg type="a(ssssx)" name="entries" direction="out"/>
    </method>
    <method name="GetTasks">
      <arg type="a(xsbii)" name="tasks" direction="out"/>
    </method>
    <method name="CancelTask">
      <arg type="x" name="task_id" direction="in"/>
    </method>
    <method name="GetTaskLog">
      <arg type="a(xssbbbsiii)" name="log" direction="out"/>
    </method>
    <method name="GetSchedule">
      <arg type="a(sbxxx)" name="schedule" direction="out"/>
    </method>
    <method name="HistoryRemove">
      <arg type="x" name="index" direction="in"/>
    </method>
    <method name="ClearHistory"/>
    <signal name="WallpaperChanged">
      <arg type="s" name="file"/>
      <arg type="s" name="source"/>
      <arg type="s" name="title"/>
    </signal>
    <signal name="FetchFailed">
      <arg type="s" name="source"/>
      <arg type="s" name="error"/>
    </signal>
    <signal name="ImageAdded">
      <arg type="s" name="source"/>
      <arg type="s" name="path"/>
    </signal>
    <signal name="TasksChanged"/>
    <signal name="TaskLogChanged"/>
  </interface>
</node>"#;

#[cfg(test)]
mod tests {
    use glib::prelude::ToVariant;

    /// D-Bus method replies are always a tuple of the out arguments: with a
    /// single out arg `a(sssx)` the reply type is `(a(sssx))` and clients must
    /// unwrap the outer tuple first. Regression: the gallery/history pages
    /// once parsed the bare array and silently got an empty list (blank pages).
    #[test]
    fn single_array_out_arg_is_wrapped_in_tuple() {
        let images: Vec<(String, String, String, i64)> = vec![
            ("/img1.jpg".into(), "Title".into(), "bing".into(), 1_700_000_000),
            ("/img2.jpg".into(), "Title 2".into(), "bing".into(), 1_700_000_001),
        ];
        let reply = (images,).to_variant();
        assert_eq!(reply.type_().to_string(), "(a(sssx))");
        // Wrong parse (old implementation): bare array → None
        assert!(reply.get::<Vec<(String, String, String, i64)>>().is_none());
        // Correct parse: unwrap the outer 1-tuple first
        let parsed = reply
            .get::<(Vec<(String, String, String, i64)>,)>()
            .map(|t| t.0)
            .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "/img1.jpg");

        // GetHistory's `(a(ssssx))` works the same way
        let h: Vec<(String, String, String, String, i64)> = vec![(
            "/img1.jpg".into(),
            "bing".into(),
            "Title".into(),
            "Copyright".into(),
            1_700_000_000,
        )];
        let reply = (h,).to_variant();
        assert_eq!(reply.type_().to_string(), "(a(ssssx))");
        assert!(reply.get::<Vec<(String, String, String, String, i64)>>().is_none());
        assert_eq!(
            reply
                .get::<(Vec<(String, String, String, String, i64)>,)>()
                .map(|t| t.0.len()),
            Some(1)
        );
    }
}
