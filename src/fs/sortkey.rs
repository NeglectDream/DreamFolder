//! 名称比较键：资源管理器「按名称排序」用的那一套。

use std::cmp::Ordering;

/// 生成自然排序键（NUL 结尾的 UTF-16）。
///
/// 资源管理器按名称排序时，比的是 `shlwapi!StrCmpLogicalW`，它的规则与
/// 任何「小写化后按字节比」都不同：
///   - 连续数字按**数值**比：`file2` 在 `file10` 之前（字典序恰好相反）；
///   - 文本段按**当前用户区域**的规则比：中文按拼音（阿 < 波 < 中），
///     而不是按 Unicode 码点（那会得到「部首笔画」序，看起来完全乱）；
///   - 忽略大小写与多数标点的权重：`ABC`/`abc` 视为同一档。
///
/// 这里直接调系统实现，不自己写「切数字段 + `to_lowercase`」的近似：
/// 那种近似在拼音、前导零（`img7`/`img007`）、全角字符上都会与资源管理器
/// 偏离，而这几处恰好是用户一眼能看出不对劲的地方。
///
/// 键**先算一遍再排序**：1 万条目约 13 万次比较，如果在比较函数里现转
/// UTF-16，就是 26 万次分配——比枚举本身还贵。
pub fn natural_key(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// 比较两个自然排序键。
///
/// `StrCmpLogicalW` 在「只有大小写/标点权重不同」时返回相等，此时退回原始
/// 字节序：既让结果完全可预测，也给 Rust 的排序一个严格全序——它对非全序
/// 比较函数是检测后直接 panic 的（`panic = "abort"` 下等同于崩溃）。
pub fn natural_cmp(a: &[u16], b: &[u16]) -> Ordering {
    use windows_sys::Win32::UI::Shell::StrCmpLogicalW;
    // SAFETY: 两个指针都来自 natural_key 产出的 NUL 结尾缓冲，
    // 在调用期间借用有效；函数只读。
    let r = unsafe { StrCmpLogicalW(a.as_ptr(), b.as_ptr()) };
    match r.cmp(&0) {
        Ordering::Equal => a.cmp(b),
        o => o,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试里方便一点：一次比较两个字符串。
    fn cmp(a: &str, b: &str) -> Ordering {
        natural_cmp(&natural_key(a), &natural_key(b))
    }

    /// 数字按数值而不是按字典序——资源管理器最典型的一条规则。
    #[test]
    fn digits_compare_numerically() {
        assert_eq!(cmp("file2.txt", "file10.txt"), Ordering::Less);
        assert_eq!(cmp("chapter 9", "chapter 10"), Ordering::Less);
        // 反例对照：字典序会认为 "file10" < "file2"
        assert!("file10.txt" < "file2.txt");
    }

    /// 纯大小写差异不能算作「无关」——必须仍然是一个严格全序，
    /// 否则排序的崩溃检查会被触发。
    #[test]
    fn case_only_difference_is_total_order() {
        let a = natural_key("Readme");
        let b = natural_key("readme");
        assert_ne!(natural_cmp(&a, &b), Ordering::Equal);
        // 反对称：比较方向相反时结论相反
        assert_eq!(natural_cmp(&a, &b), natural_cmp(&b, &a).reverse());
    }

    /// 打分充分覆盖各种输入，确认比较器是全序（不触发排序的崩溃检查）。
    #[test]
    fn sorting_arbitrary_names_does_not_violate_total_order() {
        let mut names: Vec<String> = Vec::new();
        for stem in ["a", "A", "ab", "a-b", "a_b", "a b", "2", "10", "007", "中文", "文件", "é"] {
            names.push(stem.to_string());
            names.push(format!("{stem}.txt"));
            names.push(format!("{stem} (1)"));
        }
        let mut keys: Vec<(Vec<u16>, &str)> =
            names.iter().map(|n| (natural_key(n), n.as_str())).collect();
        keys.sort_by(|a, b| natural_cmp(&a.0, &b.0));
        assert_eq!(keys.len(), names.len());
    }
}
