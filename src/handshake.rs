#[derive(Debug)]
pub struct HandshakePacket {
    pub code: u32,
    pub conv: u32,
    pub token: u32,
    pub enet: u32,
    pub end_magic: u32,
}

pub const HANDSHAKE_PACKET_LEN: usize = 20;

pub const HANDSHAKE_CODE: u32 = 255;
pub const HANDSHAKE_RET_CODE: u32 = 325;
pub const DISCONNECT_CODE: u32 = 404;
pub const HANDSHAKE_MAGIC: u32 = 0xFFFFFFFF;
pub const HANDSHAKE_RET_MAGIC: u32 = 0x14514545;
pub const DISCONNECT_MAGIC: u32 = 0x19419494;

impl HandshakePacket {
    pub fn parse(raw_data: [u8; HANDSHAKE_PACKET_LEN]) -> HandshakePacket {
        let arr = convert(raw_data);
        HandshakePacket {
            code: u32::from_be_bytes(arr[0]),
            conv: u32::from_be_bytes(arr[1]),
            token: u32::from_be_bytes(arr[2]),
            enet: u32::from_be_bytes(arr[3]),
            end_magic: u32::from_be_bytes(arr[4]),
        }
    }

    pub fn to_bytes(&self) -> [u8; HANDSHAKE_PACKET_LEN] {
        let mut ret = [0; HANDSHAKE_PACKET_LEN];
        ret[0..4].copy_from_slice(&self.code.to_be_bytes());
        ret[4..8].copy_from_slice(&self.conv.to_be_bytes());
        ret[8..12].copy_from_slice(&self.token.to_be_bytes());
        ret[12..16].copy_from_slice(&self.enet.to_be_bytes());
        ret[16..20].copy_from_slice(&self.end_magic.to_be_bytes());
        ret
    }

    pub fn is_handshake(&self) -> bool {
        self.code == HANDSHAKE_CODE && self.end_magic == HANDSHAKE_MAGIC
    }

    pub fn is_handshake_ret(&self) -> bool {
        self.code == HANDSHAKE_RET_CODE && self.end_magic == HANDSHAKE_RET_MAGIC
    }

    pub fn is_disconnect(&self) -> bool {
        self.code == DISCONNECT_CODE && self.end_magic == DISCONNECT_MAGIC
    }
}

fn convert(x: [u8; HANDSHAKE_PACKET_LEN]) -> [[u8; 4]; 5] {
    unsafe { std::mem::transmute(x) }
}
