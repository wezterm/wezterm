use crate::termwindow::render::TripleLayerQuadAllocator;
use crate::termwindow::{UIItem, UIItemType};
use mux::pane::Pane;
use mux::tab::{PositionedSplit, SplitDirection};
use std::sync::Arc;

impl crate::TermWindow {
    pub fn paint_split(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        split: &PositionedSplit,
        pane: &Arc<dyn Pane>,
    ) -> anyhow::Result<()> {
        let palette = pane.palette();
        let foreground = palette.split.to_linear();
        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;

        // pane_padding widens the reserved gap around a split beyond the
        // bare 1-cell divider; center the rendered line (and the clickable
        // hit region) within that gap rather than assuming it's always
        // exactly 1 cell wide. `leading` is how many of the gap's cells sit
        // before the divider line (on the first/top pane's side).
        let (leading, gap_cells) = self.config.pane_padding.split_gap_cells(
            split.direction == SplitDirection::Horizontal,
            cell_width,
            cell_height,
            self.dimensions.dpi as f32,
        );
        let gap_cells = gap_cells as f32;
        let leading = leading as f32;

        // The line is drawn past the ends of the split's own span so that it
        // visually connects with a perpendicular divider at a T or + junction
        // - that divider sits at the center of *its* gap, so the overshoot
        // must reach that center rather than a flat half-cell once
        // pane_padding widens gaps beyond the bare 1-cell divider. Assumes
        // symmetric padding (equal on both sides of the perpendicular gap),
        // which holds for any gutter-style config; centering on the gap's
        // total width is exact in that case.
        let (_, perp_gap_cells) = self.config.pane_padding.split_gap_cells(
            split.direction != SplitDirection::Horizontal,
            cell_width,
            cell_height,
            self.dimensions.dpi as f32,
        );
        let overshoot = perp_gap_cells as f32 / 2.0;

        let border = self.get_os_border();
        let first_row_offset = if self.show_tab_bar && !self.config.tab_bar_at_bottom {
            self.tab_bar_pixel_height()?
        } else {
            0.
        } + border.top.get() as f32;

        let (padding_left, padding_top) = self.padding_left_top();

        let pos_y = split.top as f32 * cell_height + first_row_offset + padding_top;
        let pos_x = split.left as f32 * cell_width + padding_left + border.left.get() as f32;

        if split.direction == SplitDirection::Horizontal {
            self.filled_rectangle(
                layers,
                2,
                euclid::rect(
                    pos_x + (leading + 0.5) * cell_width,
                    pos_y - (overshoot * cell_height),
                    self.render_metrics.underline_height as f32,
                    (split.size as f32 + 2.0 * overshoot) * cell_height,
                ),
                foreground,
            )?;
            self.ui_items.push(UIItem {
                x: border.left.get() as usize
                    + padding_left as usize
                    + (split.left * cell_width as usize),
                width: (gap_cells * cell_width) as usize,
                y: padding_top as usize
                    + first_row_offset as usize
                    + split.top * cell_height as usize,
                height: split.size * cell_height as usize,
                item_type: UIItemType::Split(split.clone()),
            });
        } else {
            self.filled_rectangle(
                layers,
                2,
                euclid::rect(
                    pos_x - (overshoot * cell_width),
                    pos_y + (leading + 0.5) * cell_height,
                    (split.size as f32 + 2.0 * overshoot) * cell_width,
                    self.render_metrics.underline_height as f32,
                ),
                foreground,
            )?;
            self.ui_items.push(UIItem {
                x: border.left.get() as usize
                    + padding_left as usize
                    + (split.left * cell_width as usize),
                width: split.size * cell_width as usize,
                y: padding_top as usize
                    + first_row_offset as usize
                    + split.top * cell_height as usize,
                height: (gap_cells * cell_height) as usize,
                item_type: UIItemType::Split(split.clone()),
            });
        }

        Ok(())
    }
}
