use crate::termwindow::box_model::*;
use crate::termwindow::{SidebarItem, SidebarRenameTarget, UIItemType};
use crate::utilsprites::RenderMetrics;
use config::{Dimension, DimensionContext, TabBarColors};
use mux::Mux;
use window::color::LinearRgba;

impl super::super::TermWindow {
    pub fn paint_sidebar(&mut self) -> anyhow::Result<()> {
        if !self.sidebar_visible {
            return Ok(());
        }

        // Always rebuild to reflect current state (active tab, workspace changes, etc.)
        let content = self.build_sidebar_content()?;
        let footer = self.build_sidebar_footer()?;

        let gl_state = self.render_state.as_ref().unwrap();

        let mut ui_items = content.ui_items();
        self.render_element(&content, gl_state, None)?;
        self.ui_items.append(&mut ui_items);

        let mut footer_items = footer.ui_items();
        self.render_element(&footer, gl_state, None)?;
        self.ui_items.append(&mut footer_items);

        Ok(())
    }

    fn sidebar_colors(&self) -> SidebarColors {
        let colors = self
            .config
            .colors
            .as_ref()
            .and_then(|c| c.tab_bar.as_ref())
            .cloned()
            .unwrap_or_else(TabBarColors::default);

        let bg_color = if self.focused.is_some() {
            self.config.window_frame.active_titlebar_bg
        } else {
            self.config.window_frame.inactive_titlebar_bg
        }
        .to_linear();

        let fg_color = if self.focused.is_some() {
            self.config.window_frame.active_titlebar_fg
        } else {
            self.config.window_frame.inactive_titlebar_fg
        }
        .to_linear();

        let active_tab = colors.active_tab();
        let inactive_tab = colors.inactive_tab();
        let hover_tab = colors.inactive_tab_hover();

        // Ghost color: barely visible against background
        let ghost = LinearRgba::with_components(
            bg_color.0 + 0.06,
            bg_color.1 + 0.06,
            bg_color.2 + 0.06,
            1.0,
        );

        SidebarColors {
            bg: bg_color,
            fg: fg_color,
            active_bg: active_tab.bg_color.to_linear(),
            active_fg: active_tab.fg_color.to_linear(),
            tab_fg: inactive_tab.fg_color.to_linear(),
            hover_bg: hover_tab.bg_color.to_linear(),
            hover_fg: hover_tab.fg_color.to_linear(),
            header_fg: self.config.window_frame.button_fg.to_linear(),
            border_color: colors.inactive_tab_edge().to_linear(),
            ghost,
        }
    }

    fn build_sidebar_content(&self) -> anyhow::Result<ComputedElement> {
        let font = self.fonts.title_font()?;
        let btn_font = self.fonts.command_palette_font()?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());

        let mux = Mux::get();
        let active_ws = mux.active_workspace();
        let workspaces = mux.iter_workspaces();
        let c = self.sidebar_colors();

        log::debug!(
            "sidebar: active_ws={:?}, workspaces={:?}, collapsed={:?}",
            active_ws,
            workspaces,
            self.sidebar_collapsed
        );

        // Header: "WORKSPACES"
        let mut elements = vec![
            Element::new(&font, ElementContent::Text("WORKSPACES".to_string()))
                .colors(ElementColors {
                    border: BorderColor {
                        left: LinearRgba::TRANSPARENT,
                        top: LinearRgba::TRANSPARENT,
                        right: LinearRgba::TRANSPARENT,
                        bottom: c.border_color,
                    },
                    bg: c.bg.into(),
                    text: c.header_fg.into(),
                })
                .border(BoxDimension {
                    left: Dimension::Pixels(0.0),
                    top: Dimension::Pixels(0.0),
                    right: Dimension::Pixels(0.0),
                    bottom: Dimension::Pixels(1.0),
                })
                .padding(BoxDimension {
                    left: Dimension::Cells(0.75),
                    right: Dimension::Cells(0.5),
                    top: Dimension::Cells(0.5),
                    bottom: Dimension::Cells(0.4),
                })
                .min_width(Some(Dimension::Percent(1.0)))
                .display(DisplayType::Block)
                .zindex(5),
        ];

        // Workspace rows
        for ws_name in &workspaces {
            let is_active = ws_name == &active_ws;
            let is_expanded = !self.sidebar_collapsed.contains(ws_name.as_str());
            let is_renaming = matches!(
                &self.sidebar_rename_active,
                Some(SidebarRenameTarget::Workspace(n)) if n == ws_name
            );

            let row_bg = if is_active { c.active_bg } else { c.bg };
            let row_fg = if is_active { c.active_fg } else { c.fg };

            let mut row_children = Vec::new();

            // Disclosure triangle
            let disclosure_char = if is_expanded { "\u{25BC}" } else { "\u{25B6}" };
            row_children.push(
                Element::new(&font, ElementContent::Text(disclosure_char.to_string()))
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: LinearRgba::TRANSPARENT.into(),
                        text: c.header_fg.into(),
                    })
                    .padding(BoxDimension {
                        left: Dimension::Cells(0.0),
                        right: Dimension::Cells(0.3),
                        top: Dimension::Cells(0.0),
                        bottom: Dimension::Cells(0.0),
                    })
                    .item_type(UIItemType::Sidebar(SidebarItem::WorkspaceDisclosure {
                        name: ws_name.clone(),
                    })),
            );

            // Workspace name or rename field
            if is_renaming {
                let rename_text = format!("{}|", self.sidebar_rename_buffer);
                row_children.push(
                    Element::new(&font, ElementContent::Text(rename_text))
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: LinearRgba::TRANSPARENT.into(),
                            text: c.fg.into(),
                        })
                        .item_type(UIItemType::Sidebar(SidebarItem::RenameTextField)),
                );
            } else {
                row_children.push(
                    Element::new(&font, ElementContent::Text(ws_name.clone()))
                        .colors(ElementColors {
                            border: BorderColor::default(),
                            bg: LinearRgba::TRANSPARENT.into(),
                            text: row_fg.into(),
                        })
                        .item_type(UIItemType::Sidebar(SidebarItem::WorkspaceHeader {
                            name: ws_name.clone(),
                        })),
                );
            }

            // Close button — ghost, red danger on hover (uses larger font)
            row_children.push(
                Element::new(&btn_font, ElementContent::Text("\u{2715}".to_string()))
                    .float(Float::Right)
                    .padding(BoxDimension {
                        left: Dimension::Cells(0.25),
                        right: Dimension::Cells(0.0),
                        top: Dimension::Cells(0.0),
                        bottom: Dimension::Cells(0.0),
                    })
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: LinearRgba::TRANSPARENT.into(),
                        text: c.ghost.into(),
                    })
                    .hover_colors(Some(ElementColors {
                        border: BorderColor::default(),
                        bg: LinearRgba::with_components(0.7, 0.2, 0.2, 1.0).into(),
                        text: LinearRgba::with_components(1.0, 1.0, 1.0, 1.0).into(),
                    }))
                    .item_type(UIItemType::Sidebar(SidebarItem::WorkspaceCloseButton {
                        name: ws_name.clone(),
                    })),
            );

            // New tab button (+) — ghost, brightens on hover (uses larger font)
            row_children.push(
                Element::new(&btn_font, ElementContent::Text("+".to_string()))
                    .float(Float::Right)
                    .padding(BoxDimension {
                        left: Dimension::Cells(0.25),
                        right: Dimension::Cells(0.25),
                        top: Dimension::Cells(0.0),
                        bottom: Dimension::Cells(0.0),
                    })
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: LinearRgba::TRANSPARENT.into(),
                        text: c.ghost.into(),
                    })
                    .hover_colors(Some(ElementColors {
                        border: BorderColor::default(),
                        bg: c.hover_bg.into(),
                        text: c.hover_fg.into(),
                    }))
                    .item_type(UIItemType::Sidebar(SidebarItem::NewTabButton {
                        workspace: ws_name.clone(),
                    })),
            );

            // Workspace row container
            let ws_row = Element::new(&font, ElementContent::Children(row_children))
                .colors(ElementColors {
                    border: if is_active {
                        BorderColor {
                            left: c.active_fg,
                            top: LinearRgba::TRANSPARENT,
                            right: LinearRgba::TRANSPARENT,
                            bottom: LinearRgba::TRANSPARENT,
                        }
                    } else {
                        BorderColor::default()
                    },
                    bg: row_bg.into(),
                    text: row_fg.into(),
                })
                .border(if is_active {
                    BoxDimension {
                        left: Dimension::Pixels(2.0),
                        top: Dimension::Pixels(0.0),
                        right: Dimension::Pixels(0.0),
                        bottom: Dimension::Pixels(0.0),
                    }
                } else {
                    BoxDimension::default()
                })
                .hover_colors(Some(ElementColors {
                    border: BorderColor::default(),
                    bg: c.hover_bg.into(),
                    text: c.hover_fg.into(),
                }))
                .padding(BoxDimension {
                    left: Dimension::Cells(0.5),
                    right: Dimension::Cells(0.5),
                    top: Dimension::Cells(0.4),
                    bottom: Dimension::Cells(0.4),
                })
                .line_height(Some(1.25))
                .min_width(Some(Dimension::Percent(1.0)))
                .display(DisplayType::Block)
                .zindex(5);

            elements.push(ws_row);

            // Expanded tabs
            if is_expanded {
                let wids = mux.iter_windows_in_workspace(ws_name);
                for wid in wids {
                    if let Some(window) = mux.get_window(wid) {
                        for tab in window.iter() {
                            let tab_title = tab.get_title();
                            let title = if tab_title.is_empty() {
                                tab.get_active_pane()
                                    .map(|pane| pane.get_title())
                                    .unwrap_or_else(|| "(no pane)".to_string())
                            } else {
                                tab_title
                            };
                            let tid = tab.tab_id();

                            // Check if this is the currently viewed tab
                            // (active tab in the active workspace only)
                            let is_current_tab = is_active
                                && mux
                                    .get_active_tab_for_window(wid)
                                    .map(|active| active.tab_id() == tid)
                                    .unwrap_or(false);

                            let is_tab_renaming = matches!(
                                &self.sidebar_rename_active,
                                Some(SidebarRenameTarget::Tab(id)) if *id == tid
                            );

                            let mut tab_children = Vec::new();

                            if is_tab_renaming {
                                let rename_text = format!("{}|", self.sidebar_rename_buffer);
                                tab_children.push(
                                    Element::new(&font, ElementContent::Text(rename_text))
                                        .colors(ElementColors {
                                            border: BorderColor::default(),
                                            bg: LinearRgba::TRANSPARENT.into(),
                                            text: c.fg.into(),
                                        })
                                        .item_type(UIItemType::Sidebar(SidebarItem::RenameTextField)),
                                );
                            } else {
                                // Green dot only for the tab currently being viewed
                                let activity_prefix = if is_current_tab {
                                    "\u{25CF} "
                                } else {
                                    "  "
                                };
                                let display_title =
                                    format!("{}{}", activity_prefix, title);
                                tab_children.push(
                                    Element::new(
                                        &font,
                                        ElementContent::Text(display_title),
                                    )
                                    .colors(ElementColors {
                                        border: BorderColor::default(),
                                        bg: LinearRgba::TRANSPARENT.into(),
                                        text: c.tab_fg.into(),
                                    })
                                    .item_type(UIItemType::Sidebar(SidebarItem::TabEntry {
                                        workspace: ws_name.clone(),
                                        window_id: wid,
                                        tab_id: tid,
                                    })),
                                );
                            }

                            // Tab close button — ghost, subtle on hover (larger font)
                            tab_children.push(
                                Element::new(
                                    &btn_font,
                                    ElementContent::Text("\u{2715}".to_string()),
                                )
                                .float(Float::Right)
                                .padding(BoxDimension {
                                    left: Dimension::Cells(0.3),
                                    right: Dimension::Cells(0.1),
                                    top: Dimension::Cells(0.0),
                                    bottom: Dimension::Cells(0.0),
                                })
                                .colors(ElementColors {
                                    border: BorderColor::default(),
                                    bg: LinearRgba::TRANSPARENT.into(),
                                    text: c.ghost.into(),
                                })
                                .hover_colors(Some(ElementColors {
                                    border: BorderColor::default(),
                                    bg: c.hover_bg.into(),
                                    text: c.hover_fg.into(),
                                }))
                                .item_type(UIItemType::Sidebar(SidebarItem::TabCloseButton {
                                    tab_id: tid,
                                })),
                            );

                            let tab_row =
                                Element::new(&font, ElementContent::Children(tab_children))
                                    .colors(ElementColors {
                                        border: BorderColor::default(),
                                        bg: c.bg.into(),
                                        text: c.tab_fg.into(),
                                    })
                                    .hover_colors(Some(ElementColors {
                                        border: BorderColor::default(),
                                        bg: c.hover_bg.into(),
                                        text: c.hover_fg.into(),
                                    }))
                                    .padding(BoxDimension {
                                        left: Dimension::Cells(1.5),
                                        right: Dimension::Cells(0.5),
                                        top: Dimension::Cells(0.3),
                                        bottom: Dimension::Cells(0.3),
                                    })
                                    .line_height(Some(1.25))
                                    .item_type(UIItemType::Sidebar(SidebarItem::TabEntry {
                                        workspace: ws_name.clone(),
                                        window_id: wid,
                                        tab_id: tid,
                                    }))
                                    .min_width(Some(Dimension::Percent(1.0)))
                                    .display(DisplayType::Block)
                                    .zindex(5);

                            elements.push(tab_row);
                        }
                        drop(window);
                    }
                }
            }
        }

        // Root container (scrollable content)
        let root = Element::new(&font, ElementContent::Children(elements))
            .colors(ElementColors {
                border: BorderColor {
                    left: c.border_color,
                    top: LinearRgba::TRANSPARENT,
                    right: LinearRgba::TRANSPARENT,
                    bottom: LinearRgba::TRANSPARENT,
                },
                bg: c.bg.into(),
                text: c.fg.into(),
            })
            .border(BoxDimension {
                left: Dimension::Pixels(1.0),
                top: Dimension::Pixels(0.0),
                right: Dimension::Pixels(0.0),
                bottom: Dimension::Pixels(0.0),
            })
            .min_height(Some(Dimension::Percent(1.0)))
            .item_type(UIItemType::Sidebar(SidebarItem::Background))
            .display(DisplayType::Block)
            .zindex(5);

        // Position the sidebar
        let border = self.get_os_border();
        let tab_bar_y = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };

        let h_context = DimensionContext {
            dpi: self.dimensions.dpi as f32,
            pixel_max: self.dimensions.pixel_width as f32,
            pixel_cell: metrics.cell_size.width as f32,
        };
        let sidebar_width = self.config.sidebar_width.evaluate_as_pixels(h_context);

        let pixel_width = self.dimensions.pixel_width as f32;
        let pixel_height = self.dimensions.pixel_height as f32;
        let sidebar_x = pixel_width - sidebar_width - border.right.get() as f32;
        let sidebar_y = border.top.get() as f32 + tab_bar_y;

        let mut computed = self.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: pixel_height,
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: sidebar_width,
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds: euclid::rect(0., 0., sidebar_width, pixel_height - sidebar_y),
                metrics: &metrics,
                gl_state: self.render_state.as_ref().unwrap(),
                zindex: 5,
            },
            &root,
        )?;

        computed.translate(euclid::vec2(sidebar_x, sidebar_y - self.sidebar_scroll_offset));

        Ok(computed)
    }

    fn build_sidebar_footer(&self) -> anyhow::Result<ComputedElement> {
        let font = self.fonts.title_font()?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        let c = self.sidebar_colors();

        let footer = Element::new(
            &font,
            ElementContent::Text("+ New Workspace".to_string()),
        )
        .colors(ElementColors {
            border: BorderColor {
                left: c.border_color,
                top: c.border_color,
                right: LinearRgba::TRANSPARENT,
                bottom: LinearRgba::TRANSPARENT,
            },
            bg: c.bg.into(),
            text: c.header_fg.into(),
        })
        .border(BoxDimension {
            left: Dimension::Pixels(1.0),
            top: Dimension::Pixels(1.0),
            right: Dimension::Pixels(0.0),
            bottom: Dimension::Pixels(0.0),
        })
        .hover_colors(Some(ElementColors {
            border: BorderColor::default(),
            bg: c.hover_bg.into(),
            text: c.hover_fg.into(),
        }))
        .padding(BoxDimension {
            left: Dimension::Cells(0.75),
            right: Dimension::Cells(0.5),
            top: Dimension::Cells(0.5),
            bottom: Dimension::Cells(0.5),
        })
        .min_width(Some(Dimension::Percent(1.0)))
        .item_type(UIItemType::Sidebar(SidebarItem::NewWorkspaceButton))
        .display(DisplayType::Block)
        .zindex(5);

        // Position at bottom of sidebar
        let border = self.get_os_border();
        let h_context = DimensionContext {
            dpi: self.dimensions.dpi as f32,
            pixel_max: self.dimensions.pixel_width as f32,
            pixel_cell: metrics.cell_size.width as f32,
        };
        let sidebar_width = self.config.sidebar_width.evaluate_as_pixels(h_context);

        let pixel_width = self.dimensions.pixel_width as f32;
        let pixel_height = self.dimensions.pixel_height as f32;
        let sidebar_x = pixel_width - sidebar_width - border.right.get() as f32;

        // Compute the footer element to get its height
        let layout_ctx = LayoutContext {
            height: DimensionContext {
                dpi: self.dimensions.dpi as f32,
                pixel_max: pixel_height,
                pixel_cell: metrics.cell_size.height as f32,
            },
            width: DimensionContext {
                dpi: self.dimensions.dpi as f32,
                pixel_max: sidebar_width,
                pixel_cell: metrics.cell_size.width as f32,
            },
            bounds: euclid::rect(0., 0., sidebar_width, pixel_height),
            metrics: &metrics,
            gl_state: self.render_state.as_ref().unwrap(),
            zindex: 5,
        };

        let mut computed = self.compute_element(&layout_ctx, &footer)?;
        let footer_height = computed.bounds.height();
        let footer_y = pixel_height - footer_height - border.bottom.get() as f32;

        computed.translate(euclid::vec2(sidebar_x, footer_y));

        Ok(computed)
    }
}

/// Colors used throughout the sidebar, derived from config theme.
struct SidebarColors {
    bg: LinearRgba,
    fg: LinearRgba,
    active_bg: LinearRgba,
    active_fg: LinearRgba,
    tab_fg: LinearRgba,
    hover_bg: LinearRgba,
    hover_fg: LinearRgba,
    header_fg: LinearRgba,
    border_color: LinearRgba,
    ghost: LinearRgba,
}
