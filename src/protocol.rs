// Graphwar network protocol: one line per message, fields separated by '&'.
// Text fields are Java URLEncoder encoded (space -> '+', bytes -> %XX).

pub const NO_INFO: i32 = 10;
pub const CHAT_MSG: i32 = 14;
pub const ADD_PLAYER: i32 = 16;
pub const ADD_SOLDIER: i32 = 17;
pub const REMOVE_SOLDIER: i32 = 19;
pub const SET_TEAM: i32 = 20;
pub const SET_READY: i32 = 21;
pub const START_GAME: i32 = 22;
pub const FIRE_FUNC: i32 = 24;
pub const NEXT_TURN: i32 = 25;
pub const READY_NEXT_TURN: i32 = 27;
pub const SET_ANGLE: i32 = 28;
pub const REMOVE_PLAYER: i32 = 29;
pub const NEXT_MODE: i32 = 31;
pub const SET_MODE: i32 = 33;
pub const TIME_UP: i32 = 37;
pub const GAME_FULL: i32 = 38;
pub const DISCONNECT: i32 = 39;
pub const GAME_FINISHED: i32 = 40;
pub const NEW_LEADER: i32 = 41;
pub const START_COUNTDOWN: i32 = 42;
pub const REORDER: i32 = 43;
pub const FUNCTION_PREVIEW: i32 = 44;

pub const NORMAL_FUNC: i32 = 0;
pub const FST_ODE: i32 = 1;
pub const SND_ODE: i32 = 2;

pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            x => {
                out.push(x);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn split(line: &str) -> Vec<String> {
    line.trim_end_matches(['\r', '\n'])
        .split('&')
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_roundtrip() {
        let s = "0.2*x + 7*exp(0-((x-3)^2)/8) ы";
        assert_eq!(url_decode(&url_encode(s)), s);
        assert_eq!(url_encode("a b+c"), "a+b%2Bc");
    }
}
