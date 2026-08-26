use std::str::FromStr;

use cubic_network::{DEFAULT_MINECRAFT_PORT, ServerAddress, ServerAddressError};

#[test]
fn supported_server_address_forms_parse_strictly() {
    for (input, host, port, display) in [
        (
            "localhost",
            "localhost",
            DEFAULT_MINECRAFT_PORT,
            "localhost:25565",
        ),
        ("localhost:25565", "localhost", 25_565, "localhost:25565"),
        ("example.org", "example.org", 25_565, "example.org:25565"),
        (
            "example.org:12345",
            "example.org",
            12_345,
            "example.org:12345",
        ),
        ("127.0.0.1", "127.0.0.1", 25_565, "127.0.0.1:25565"),
        ("127.0.0.1:25565", "127.0.0.1", 25_565, "127.0.0.1:25565"),
        ("[::1]", "::1", 25_565, "[::1]:25565"),
        ("[::1]:25565", "::1", 25_565, "[::1]:25565"),
    ] {
        let address = ServerAddress::from_str(input).unwrap();
        assert_eq!(address.host(), host);
        assert_eq!(address.port(), port);
        assert_eq!(address.to_string(), display);
    }
}

#[test]
fn malformed_server_addresses_are_rejected_without_trimming() {
    for input in [
        "",
        ":25565",
        "host:",
        "host:notaport",
        "host:99999",
        "host:0",
        "[::1",
        "[]:25565",
        "::1",
        " localhost",
        "localhost ",
        "local host",
    ] {
        assert!(
            ServerAddress::from_str(input).is_err(),
            "accepted {input:?}"
        );
    }
    assert_eq!(
        ServerAddress::from_str("host:0"),
        Err(ServerAddressError::ZeroPort)
    );
}
