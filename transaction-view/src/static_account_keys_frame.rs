use {
    crate::{
        bytes::{advance_offset_for_array, optimized_read_compressed_u16},
        result::{Result, TransactionViewError},
    },
    solana_packet::PACKET_DATA_SIZE,
    solana_pubkey::Pubkey,
};

pub const MAX_STATIC_ACCOUNTS_PER_PACKET: usize = {
    let max_static_accounts = PACKET_DATA_SIZE / core::mem::size_of::<Pubkey>();
    if max_static_accounts > u8::MAX as usize {
        u8::MAX as usize
    } else {
        max_static_accounts
    }
};

/// Contains metadata about the static account keys in a transaction packet.
#[derive(Debug, Default)]
pub(crate) struct StaticAccountKeysFrame {
    /// The number of static accounts in the transaction.
    pub(crate) num_static_accounts: u8,
    /// The offset to the first static account in the transaction.
    pub(crate) offset: u16,
}

impl StaticAccountKeysFrame {
    #[inline(always)]
    pub(crate) fn try_new(bytes: &[u8], offset: &mut usize) -> Result<Self> {
        let num_static_accounts = optimized_read_compressed_u16(bytes, offset)?;
        if num_static_accounts == 0
            || usize::from(num_static_accounts) > MAX_STATIC_ACCOUNTS_PER_PACKET
        {
            return Err(TransactionViewError::ParseError);
        }
        let num_static_accounts =
            u8::try_from(num_static_accounts).map_err(|_| TransactionViewError::ParseError)?;

        // We also know that the offset must be less than 3 here, since the
        // compressed u16 can only use up to 3 bytes, so there is no need to
        // check if the offset is greater than u16::MAX.
        let static_accounts_offset = *offset as u16;
        // Update offset for array of static accounts.
        advance_offset_for_array::<Pubkey>(bytes, offset, u16::from(num_static_accounts))?;

        Ok(Self {
            num_static_accounts,
            offset: static_accounts_offset,
        })
    }
}

#[cfg(test)]
mod tests {
    use {super::*, solana_short_vec::ShortVec};

    #[test]
    fn test_zero_accounts() {
        let bytes = bincode::serialize(&ShortVec(Vec::<Pubkey>::new())).unwrap();
        let mut offset = 0;
        assert!(StaticAccountKeysFrame::try_new(&bytes, &mut offset).is_err());
    }

    #[test]
    fn test_one_account() {
        let bytes = bincode::serialize(&ShortVec(vec![Pubkey::default()])).unwrap();
        let mut offset = 0;
        let frame = StaticAccountKeysFrame::try_new(&bytes, &mut offset).unwrap();
        assert_eq!(frame.num_static_accounts, 1);
        assert_eq!(frame.offset, 1);
        assert_eq!(offset, 1 + core::mem::size_of::<Pubkey>());
    }

    #[test]
    fn test_max_accounts() {
        let signatures = vec![Pubkey::default(); MAX_STATIC_ACCOUNTS_PER_PACKET];
        let bytes = bincode::serialize(&ShortVec(signatures)).unwrap();
        let mut offset = 0;
        let frame = StaticAccountKeysFrame::try_new(&bytes, &mut offset).unwrap();
        assert_eq!(
            usize::from(frame.num_static_accounts),
            MAX_STATIC_ACCOUNTS_PER_PACKET
        );
        assert_eq!(offset, bytes.len());
    }

    #[test]
    fn test_too_many_accounts() {
        let signatures = vec![Pubkey::default(); MAX_STATIC_ACCOUNTS_PER_PACKET + 1];
        let bytes = bincode::serialize(&ShortVec(signatures)).unwrap();
        let mut offset = 0;
        assert!(StaticAccountKeysFrame::try_new(&bytes, &mut offset).is_err());
    }

    #[test]
    fn test_u16_max_accounts() {
        let signatures = vec![Pubkey::default(); u16::MAX as usize];
        let bytes = bincode::serialize(&ShortVec(signatures)).unwrap();
        let mut offset = 0;
        assert!(StaticAccountKeysFrame::try_new(&bytes, &mut offset).is_err());
    }
}
