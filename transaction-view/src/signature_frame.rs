use {
    crate::{
        bytes::{advance_offset_for_array, optimized_read_compressed_u16},
        result::{Result, TransactionViewError},
    },
    solana_packet::PACKET_DATA_SIZE,
    solana_pubkey::Pubkey,
    solana_signature::Signature,
};

// Each signature must be paired with a unique static pubkey, so each signature
// requires the bytes for one signature and one pubkey.
const MAX_SIGNATURES_PER_PACKET: usize = {
    let max_signatures =
        PACKET_DATA_SIZE / (core::mem::size_of::<Signature>() + core::mem::size_of::<Pubkey>());
    if max_signatures > u8::MAX as usize {
        u8::MAX as usize
    } else {
        max_signatures
    }
};

/// Metadata for accessing transaction-level signatures in a transaction view.
#[derive(Debug)]
pub(crate) struct SignatureFrame {
    /// The number of signatures in the transaction.
    pub(crate) num_signatures: u8,
    /// Offset to the first signature in the transaction packet.
    pub(crate) offset: u16,
}

impl SignatureFrame {
    /// Get the number of signatures and the offset to the first signature in
    /// the transaction packet, starting at the given `offset`.
    #[inline(always)]
    pub(crate) fn try_new(bytes: &[u8], offset: &mut usize) -> Result<Self> {
        let num_signatures = optimized_read_compressed_u16(bytes, offset)?;
        if num_signatures == 0 || usize::from(num_signatures) > MAX_SIGNATURES_PER_PACKET {
            return Err(TransactionViewError::ParseError);
        }
        let num_signatures =
            u8::try_from(num_signatures).map_err(|_| TransactionViewError::ParseError)?;

        let signature_offset = *offset as u16;
        advance_offset_for_array::<Signature>(bytes, offset, u16::from(num_signatures))?;

        Ok(Self {
            num_signatures,
            offset: signature_offset,
        })
    }
}

#[cfg(test)]
mod tests {
    use {super::*, solana_short_vec::ShortVec};

    #[test]
    fn test_zero_signatures() {
        let bytes = bincode::serialize(&ShortVec(Vec::<Signature>::new())).unwrap();
        let mut offset = 0;
        assert!(SignatureFrame::try_new(&bytes, &mut offset).is_err());
    }

    #[test]
    fn test_one_signature() {
        let bytes = bincode::serialize(&ShortVec(vec![Signature::default()])).unwrap();
        let mut offset = 0;
        let frame = SignatureFrame::try_new(&bytes, &mut offset).unwrap();
        assert_eq!(frame.num_signatures, 1);
        assert_eq!(frame.offset, 1);
        assert_eq!(offset, 1 + core::mem::size_of::<Signature>());
    }

    #[test]
    fn test_max_signatures() {
        let signatures = vec![Signature::default(); MAX_SIGNATURES_PER_PACKET];
        let bytes = bincode::serialize(&ShortVec(signatures)).unwrap();
        let mut offset = 0;
        let frame = SignatureFrame::try_new(&bytes, &mut offset).unwrap();
        assert_eq!(usize::from(frame.num_signatures), MAX_SIGNATURES_PER_PACKET);
        assert_eq!(offset, bytes.len());
    }

    #[test]
    fn test_non_zero_offset() {
        let mut bytes = bincode::serialize(&ShortVec(vec![Signature::default()])).unwrap();
        bytes.insert(0, 0); // Insert a byte at the beginning of the packet.
        let mut offset = 1; // Start at the second byte.
        let frame = SignatureFrame::try_new(&bytes, &mut offset).unwrap();
        assert_eq!(frame.num_signatures, 1);
        assert_eq!(frame.offset, 2);
        assert_eq!(offset, 2 + core::mem::size_of::<Signature>());
    }

    #[test]
    fn test_too_many_signatures() {
        let signatures = vec![Signature::default(); MAX_SIGNATURES_PER_PACKET + 1];
        let bytes = bincode::serialize(&ShortVec(signatures)).unwrap();
        let mut offset = 0;
        assert!(SignatureFrame::try_new(&bytes, &mut offset).is_err());
    }

    #[test]
    fn test_u16_max_signatures() {
        let signatures = vec![Signature::default(); u16::MAX as usize];
        let bytes = bincode::serialize(&ShortVec(signatures)).unwrap();
        let mut offset = 0;
        assert!(SignatureFrame::try_new(&bytes, &mut offset).is_err());
    }
}
