pub(crate) fn prefer_swrast() -> bool {
    let config = config::configuration();

    // When the GDI text front_end is active we don't use the OpenGL path at
    // all, so software selection is irrelevant here.
    if config.front_end == config::FrontEndSelection::Gdi {
        return false;
    }

    #[cfg(windows)]
    {
        if config.front_end == config::FrontEndSelection::OpenGL
            && crate::os::windows::is_running_in_rdp_session()
        {
            // Historically we forced software rendering in RDP because OpenGL
            // has problematic behavior upon disconnect. Now that an unset
            // front_end auto-selects the GDI renderer in RDP, an OpenGL
            // selection here is necessarily explicit, and an explicit choice
            // must win. Warn but honor it.
            log::warn!(
                "front_end=\"OpenGL\" in an RDP session may behave poorly on \
                 disconnect; consider front_end=\"Gdi\" or \"Software\""
            );
        }
    }
    config.front_end == config::FrontEndSelection::Software
}
