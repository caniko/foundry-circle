use foundry_circle::driver::WorldState;

#[test]
fn stopped_world_uses_the_public_wire_name() {
    let encoded = serde_json::to_string(&WorldState::WorldStopped).unwrap();
    assert_eq!(encoded, "\"world-stopped\"");
    let decoded: WorldState = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, WorldState::WorldStopped);
}
