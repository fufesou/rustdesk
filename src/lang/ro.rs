lazy_static::lazy_static! {
pub static ref T: std::collections::HashMap<&'static str, &'static str> =
    [
        ("preset-password-in-use-tip", "Se folosește o parolă prestabilită. Se recomandă setarea unei parole personalizate pentru securitate sporită."),
    ].iter().cloned().collect();
}
