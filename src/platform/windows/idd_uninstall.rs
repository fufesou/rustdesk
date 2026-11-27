pub(crate) fn uninstall(reboot: &mut bool) -> hbb_common::ResultType<()> {
    let (_, _, _, installed_exe) = super::get_install_info();
    virtual_display::uninstall_test_driver_for(std::path::Path::new(&installed_exe), reboot)
}
