use super::chain::Chain;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AddressSimilarity {
    pub(crate) prefix_len: usize,
    pub(crate) suffix_len: usize,
}

pub(crate) fn compare(left: &str, right: &str, chain: Chain) -> AddressSimilarity {
    let (left, right, case_sensitive) = if chain == Chain::Solana {
        (left, right, true)
    } else {
        (strip_evm_prefix(left), strip_evm_prefix(right), false)
    };
    let common = |left: char, right: char| {
        if case_sensitive { left == right } else { left.eq_ignore_ascii_case(&right) }
    };

    let left_len = left.chars().count();
    let right_len = right.chars().count();
    let prefix_len =
        left.chars().zip(right.chars()).take_while(|(left, right)| common(*left, *right)).count();
    let remaining = left_len.min(right_len) - prefix_len;
    let suffix_len = left
        .chars()
        .rev()
        .zip(right.chars().rev())
        .take(remaining)
        .take_while(|(left, right)| common(*left, *right))
        .count();

    AddressSimilarity { prefix_len, suffix_len }
}

fn strip_evm_prefix(address: &str) -> &str {
    address.strip_prefix("0x").or_else(|| address.strip_prefix("0X")).unwrap_or(address)
}

#[cfg(test)]
mod tests {
    use super::{AddressSimilarity, compare};
    use crate::domain::chain::Chain;

    #[test]
    fn evm_comparison_ignores_prefix_and_case() {
        let result = compare(
            "0xd060f448de7bccb6e54b0cb8d5bc871c8d339eec",
            "0xd0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC",
            Chain::RobinhoodChain,
        );
        assert_eq!(result, AddressSimilarity { prefix_len: 4, suffix_len: 4 });
    }

    #[test]
    fn solana_comparison_is_case_sensitive() {
        assert_eq!(
            compare("Abcdef", "abcdef", Chain::Solana),
            AddressSimilarity { prefix_len: 0, suffix_len: 5 },
        );
    }

    #[test]
    fn unrelated_addresses_have_no_common_ends() {
        assert_eq!(
            compare("abc", "XYZ", Chain::Solana),
            AddressSimilarity { prefix_len: 0, suffix_len: 0 },
        );
    }

    #[test]
    fn equal_addresses_do_not_double_count_the_middle() {
        assert_eq!(
            compare("0xabc", "0xABC", Chain::Ethereum),
            AddressSimilarity { prefix_len: 3, suffix_len: 0 },
        );
    }
}
