//! 列表视口的公共逻辑。

/// 让选中项留在可视区内的滚动窗口，返回首行下标。
///
/// `offset` 是上一帧的窗口起点（由 ui 层持有），这里只做最小位移：
/// 选中项在上方就贴顶、在下方就翻页，其余情况保持不动。
pub fn window(offset: &mut usize, selected: usize, len: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    let max_first = len.saturating_sub(height);
    if selected < *offset {
        *offset = selected;
    } else if selected >= *offset + height {
        *offset = selected + 1 - height;
    }
    *offset = (*offset).min(max_first);
    *offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 滚动窗口跟着选中项() {
        let mut offset = 0;
        assert_eq!(window(&mut offset, 0, 100, 10), 0);
        assert_eq!(window(&mut offset, 25, 100, 10), 16);
        assert_eq!(window(&mut offset, 5, 100, 10), 5);
        let mut offset2 = 0;
        assert_eq!(window(&mut offset2, 99, 100, 10), 90);
        assert_eq!(window(&mut offset2, 99, 3, 10), 0);
    }
}
