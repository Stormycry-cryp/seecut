//! Local graph gestures. Preview changes never cross the durable command boundary.
use super::*;
use std::collections::BTreeSet;
#[derive(Default)]
pub(super) struct Editor {
    pub selected: BTreeSet<String>,
    pub edge: Option<Edge>,
    pub wire: Option<Wire>,
    pub size: [f64; 2],
    search: String,
}
pub(super) struct Wire {
    from: String,
    old: Option<Edge>,
    point: [f64; 2],
    target: Option<Edge>,
}
impl Host {
    fn editable(&self, token: &str) -> bool {
        self.controller.token_matches(token)
            && !self.browsing
            && self.dialog.is_none()
            && !self.controller.pending()
            && !self.recovery
            && self
                .controller
                .session
                .as_ref()
                .is_some_and(|s| !s.store.is_read_only())
    }
    pub(super) fn select_nodes(&mut self, ids: BTreeSet<String>, focus: bool) -> Result<()> {
        self.controller.save_draft()?;
        let primary = self
            .controller
            .session
            .as_ref()
            .filter(|s| ids.contains(&s.selected))
            .map(|s| s.selected.clone())
            .or_else(|| ids.iter().next().cloned())
            .unwrap_or_default();
        self.request(Intent::Select(primary))?;
        self.editor.selected = ids;
        self.editor.edge = None;
        if focus {
            self.frame_selection(false)?;
        }
        Ok(())
    }
    pub(super) fn frame_selection(&mut self, all: bool) -> Result<()> {
        if self.editor.size[0] < 200. || self.editor.size[1] < 200. {
            return Ok(());
        }
        let s = self.controller.session.as_ref().ok_or("请先打开工程")?;
        let nodes: Vec<_> = s
            .graph()
            .nodes
            .values()
            .filter(|n| all || self.editor.selected.contains(&n.id))
            .collect();
        if nodes.is_empty() {
            return Ok(());
        }
        let min_x = nodes
            .iter()
            .map(|n| n.position[0])
            .fold(f64::INFINITY, f64::min);
        let min_y = nodes
            .iter()
            .map(|n| n.position[1])
            .fold(f64::INFINITY, f64::min);
        let max_x = nodes
            .iter()
            .map(|n| n.position[0] + 228.)
            .fold(f64::NEG_INFINITY, f64::max);
        let max_y = nodes
            .iter()
            .map(|n| n.position[1] + 190.)
            .fold(f64::NEG_INFINITY, f64::max);
        let w = (self.editor.size[0] - 64.).max(280.);
        let h = (self.editor.size[1] - 148.).max(280.);
        let attached = (self.editor.selected.len() == 1)
            .then(|| s.graph().nodes.get(&s.selected))
            .flatten();
        let card_height = attached
            .map_or(0_f64, |n| match n.kind.as_str() {
                "prompt" => 210.,
                "image_generate" | "video_generate" => {
                    if s.store
                        .execution()
                        .attempts
                        .iter()
                        .any(|a| a.draft.context.node_id == n.id)
                    {
                        434.
                    } else {
                        342.
                    }
                }
                _ => 280.,
            })
            .min((self.editor.size[1] - 144.).max(0.));
        let card_width = 480_f64.min(w);
        let mut z = if all {
            (w / (max_x - min_x))
                .min(h / (max_y - min_y))
                .clamp(0.25, 1.)
        } else {
            self.view[2].min(((h - card_height) / 202.).max(0.25))
        };
        if all && let Some(n) = attached {
            z = z.min(((h - card_height) / (n.position[1] - min_y + 202.)).max(0.25));
            let dx = n.position[0] - min_x;
            if dx > 0. {
                z = z.min(((w - card_width) / dx).max(0.25));
            }
        }
        let content_width = attached.map_or((max_x - min_x) * z, |n| {
            ((max_x - min_x) * z).max((n.position[0] - min_x) * z + card_width)
        });
        let content_height = attached.map_or((max_y - min_y) * z, |n| {
            ((max_y - min_y) * z).max((n.position[1] - min_y + 202.) * z + card_height)
        });
        let view = if all {
            [
                (w - content_width) / 2. + 24. - min_x * z,
                (h - content_height) / 2. + 24. - min_y * z,
                z,
            ]
        } else {
            let mut p = self.view;
            p[2] = z;
            let x = min_x * z + p[0];
            let y = min_y * z + p[1];
            if x < 24. {
                p[0] += 24. - x;
            } else if x + card_width > w {
                p[0] -= x + card_width - w;
            }
            if y < 16. {
                p[1] += 16. - y;
            } else if y + 202. * z + card_height > h + 32. {
                p[1] -= y + 202. * z + card_height - h - 32.;
            }
            p
        };
        self.controller.preview_viewport(&s.token.clone(), view)?;
        self.controller.flush_viewport()?;
        self.view = view;
        Ok(())
    }
    fn wire_target(&self, from: &str, old: Option<&Edge>, point: [f64; 2]) -> Option<Edge> {
        let s = self.controller.session.as_ref()?;
        let [px, py, z] = self.view;
        for n in s.graph().nodes.values() {
            for (i, port) in n.ports().ok()?.0.iter().enumerate() {
                let p = self.preview.get(&n.id).unwrap_or(&n.position);
                let x = p[0] * z + px;
                let y = (p[1] + 82. + 36. * i as f64) * z + py;
                if (point[0] - x).hypot(point[1] - y) > 22. {
                    continue;
                }
                let mut g = s.graph().clone();
                if let Some(old) = old {
                    g.edges.retain(|e| e != old);
                }
                let order = g
                    .edges
                    .iter()
                    .filter(|e| e.to == n.id && e.input == port.name)
                    .map(|e| e.order)
                    .max()
                    .map_or(0, |x| x.saturating_add(1));
                let edge = Edge {
                    from: from.into(),
                    output: "out".into(),
                    to: n.id.clone(),
                    input: port.name.into(),
                    order,
                };
                g.edges.push(edge.clone());
                if g.validate(false).is_ok() {
                    return Some(edge);
                }
            }
        }
        None
    }
    fn edge_points(&self, e: &Edge) -> Option<[[f64; 2]; 4]> {
        let s = self.controller.session.as_ref()?;
        let a = s.graph().nodes.get(&e.from)?;
        let b = s.graph().nodes.get(&e.to)?;
        let a = self.preview.get(&a.id).unwrap_or(&a.position);
        let pos = self.preview.get(&b.id).unwrap_or(&b.position);
        let i = b.ports().ok()?.0.iter().position(|p| p.name == e.input)? as f64;
        let [px, py, z] = self.view;
        let x1 = (a[0] + 228.) * z + px;
        let y1 = (a[1] + 100.) * z + py;
        let x2 = pos[0] * z + px;
        let y2 = (pos[1] + 82. + 36. * i) * z + py;
        let bend = (x2 - x1).abs().max(80.) * 0.45;
        Some([[x1, y1], [x1 + bend, y1], [x2 - bend, y2], [x2, y2]])
    }
    fn hit_edge(&self, point: [f64; 2]) -> Option<Edge> {
        let s = self.controller.session.as_ref()?;
        s.graph()
            .edges
            .iter()
            .rev()
            .find(|e| {
                self.edge_points(e).is_some_and(|p| {
                    (0..=40).any(|i| {
                        let t = f64::from(i) / 40.;
                        let u = 1. - t;
                        let x = u * u * u * p[0][0]
                            + 3. * u * u * t * p[1][0]
                            + 3. * u * t * t * p[2][0]
                            + t * t * t * p[3][0];
                        let y = u * u * u * p[0][1]
                            + 3. * u * u * t * p[1][1]
                            + 3. * u * t * t * p[2][1]
                            + t * t * t * p[3][1];
                        (point[0] - x).hypot(point[1] - y) < 9.
                    })
                })
            })
            .cloned()
    }
    pub(super) fn publish_editor(&mut self, app: &App) {
        let ui = app.global::<Flow>();
        if let Some(s) = self.controller.session.as_ref() {
            self.editor
                .selected
                .retain(|id| s.graph().nodes.contains_key(id));
            if !s.selected.is_empty() && !self.editor.selected.contains(&s.selected) {
                self.editor.selected = BTreeSet::from([s.selected.clone()]);
            }
            if s.selected.is_empty() {
                self.editor.selected.clear();
            }
            if self
                .editor
                .edge
                .as_ref()
                .is_some_and(|e| !s.graph().edges.contains(e))
            {
                self.editor.edge = None;
            }
            if let Some(n) = s.graph().nodes.get(&s.selected) {
                let p = self.preview.get(&n.id).unwrap_or(&n.position);
                ui.set_selected_x(p[0] as f32);
                ui.set_selected_y(p[1] as f32);
            }
        } else {
            self.editor.selected.clear();
            self.editor.edge = None;
            self.editor.wire = None;
        }
        ui.set_selection_count(self.editor.selected.len() as i32);
        ui.set_edge_selected(self.editor.edge.is_some());
        let mut path = String::new();
        let mut target = String::new();
        if let Some(w) = &self.editor.wire
            && let Some(n) = self
                .controller
                .session
                .as_ref()
                .and_then(|s| s.graph().nodes.get(&w.from))
        {
            let p = self.preview.get(&n.id).unwrap_or(&n.position);
            let [px, py, z] = self.view;
            let x = (p[0] + 228.) * z + px;
            let y = (p[1] + 100.) * z + py;
            let [tx, ty] = w.point;
            let bend = (tx - x).abs().max(80.) * 0.45;
            path = format!("M {x} {y} C {} {y} {} {ty} {tx} {ty}", x + bend, tx - bend);
            if let Some(e) = &w.target {
                target = format!("{}:{}", e.to, e.input);
            }
        }
        ui.set_wire_path(path.into());
        ui.set_wire_target(target.into());
    }
}
pub(super) fn bind(app: &App, host: &Rc<RefCell<Host>>) {
    let ui = app.global::<Flow>();
    ui.on_canvas_size({
        let host = host.clone();
        move |w, h| {
            host.borrow_mut().editor.size = [f64::from(w), f64::from(h)];
        }
    });
    ui.on_select_node({
        let host = host.clone();
        let weak = app.as_weak();
        move |token, id, add| {
            let Some(app) = weak.upgrade() else { return };
            let mut h = host.borrow_mut();
            if !h.controller.token_matches(&token) {
                return;
            }
            if !add && h.editor.selected.contains(id.as_str()) {
                h.editor.edge = None;
                h.publish(&app);
                return;
            }
            let mut ids = if add {
                h.editor.selected.clone()
            } else {
                BTreeSet::new()
            };
            if !ids.remove(id.as_str()) && !id.is_empty() {
                ids.insert(id.to_string());
            }
            let result = h.select_nodes(ids, false);
            drop(h);
            finish(&app, &host, result.map(|()| None));
        }
    });
    ui.on_canvas_select({
        let host = host.clone();
        let weak = app.as_weak();
        move |token, x, y, x2, y2, add| {
            let Some(app) = weak.upgrade() else { return };
            let mut h = host.borrow_mut();
            if !h.controller.token_matches(&token) {
                return;
            }
            let p = [f64::from(x), f64::from(y)];
            let q = [f64::from(x2), f64::from(y2)];
            if (p[0] - q[0]).hypot(p[1] - q[1]) < 5.
                && let Some(edge) = h.hit_edge(p)
            {
                h.editor.edge = Some(edge);
                h.publish(&app);
                return;
            }
            let [px, py, z] = h.view;
            let mut ids = if add {
                h.editor.selected.clone()
            } else {
                BTreeSet::new()
            };
            if let Some(s) = h.controller.session.as_ref() {
                for n in s.graph().nodes.values() {
                    let x = n.position[0] * z + px;
                    let y = n.position[1] * z + py;
                    if x + 228. * z >= p[0].min(q[0])
                        && x <= p[0].max(q[0])
                        && y + 190. * z >= p[1].min(q[1])
                        && y <= p[1].max(q[1])
                    {
                        ids.insert(n.id.clone());
                    }
                }
            }
            let result = h.select_nodes(ids, false);
            drop(h);
            finish(&app, &host, result.map(|()| None));
        }
    });
    ui.on_wire_gesture({
        let host = host.clone();
        let weak = app.as_weak();
        move |token, id, port, phase, x, y| {
            let Some(app) = weak.upgrade() else { return };
            let mut h = host.borrow_mut();
            if !h.editable(&token) {
                return;
            }
            let point = [f64::from(x), f64::from(y)];
            if phase == "start" {
                let old = if port.is_empty() {
                    None
                } else {
                    h.editor
                        .edge
                        .clone()
                        .filter(|e| e.to == id.as_str() && e.input == port.as_str())
                        .or_else(|| {
                            h.controller.session.as_ref().and_then(|s| {
                                let es: Vec<_> = s
                                    .graph()
                                    .edges
                                    .iter()
                                    .filter(|e| e.to == id.as_str() && e.input == port.as_str())
                                    .collect();
                                (es.len() == 1).then(|| es[0].clone())
                            })
                        })
                };
                let from = if port.is_empty() {
                    id.to_string()
                } else {
                    old.as_ref().map(|e| e.from.clone()).unwrap_or_default()
                };
                if !from.is_empty() {
                    h.editor.wire = Some(Wire {
                        from,
                        old,
                        point,
                        target: None,
                    });
                }
            } else if phase == "cancel" {
                h.editor.wire = None;
            } else if let Some(mut wire) = h.editor.wire.take() {
                wire.point = point;
                wire.target = h.wire_target(&wire.from, wire.old.as_ref(), point);
                if phase == "end" {
                    if let Some(new) = wire.target {
                        let cmd = if let Some(old) = wire.old {
                            Command::Rewire { old, new }
                        } else {
                            Command::Connect { edge: new }
                        };
                        if let Err(e) = h
                            .controller
                            .save_draft()
                            .and_then(|()| h.request(Intent::Edit(Box::new(cmd))).map(|_| ()))
                        {
                            h.fail(e);
                        }
                    }
                } else {
                    h.editor.wire = Some(wire);
                }
            }
            h.publish_editor(&app);
            h.publish_edges();
            if phase == "end" {
                h.publish(&app);
            }
        }
    });
    ui.on_reference_action({
        let host = host.clone();
        let weak = app.as_weak();
        move |id, action| {
            let Some(app) = weak.upgrade() else { return };
            let mut h = host.borrow_mut();
            let result = (|| -> Result<()> {
                if action == "open" {
                    return h.select_nodes(BTreeSet::from([id.to_string()]), true);
                }
                h.controller.save_draft()?;
                let s = h.controller.session.as_ref().ok_or("请先打开工程")?;
                let edge = s
                    .graph()
                    .edges
                    .iter()
                    .find(|e| {
                        e.to == s.selected && e.from == id.as_str() && e.input == "references"
                    })
                    .cloned()
                    .ok_or("参考连线已变化")?;
                let cmd = if action == "remove" {
                    Command::Disconnect { edge }
                } else {
                    let mut edges: Vec<_> = s
                        .graph()
                        .edges
                        .iter()
                        .filter(|e| e.to == s.selected && e.input == "references")
                        .collect();
                    edges.sort_by_key(|e| e.order);
                    let mut sources: Vec<_> = edges.iter().map(|e| e.from.clone()).collect();
                    let i = sources
                        .iter()
                        .position(|n| n == id.as_str())
                        .ok_or("参考已变化")?;
                    let j = if action == "earlier" {
                        i.saturating_sub(1)
                    } else {
                        (i + 1).min(sources.len() - 1)
                    };
                    sources.swap(i, j);
                    Command::Reorder {
                        node: s.selected.clone(),
                        input: "references".into(),
                        sources,
                    }
                };
                h.request(Intent::Edit(Box::new(cmd)))?;
                Ok(())
            })();
            drop(h);
            finish(&app, &host, result.map(|()| None));
        }
    });
    ui.on_find_node({
        let host = host.clone();
        let weak = app.as_weak();
        move |query| {
            let Some(app) = weak.upgrade() else { return };
            let mut h = host.borrow_mut();
            let q = query.trim().to_lowercase();
            if q.is_empty() {
                return;
            }
            let id = h.controller.session.as_ref().and_then(|s| {
                let matches: Vec<_> = s
                    .graph()
                    .nodes
                    .values()
                    .filter(|n| {
                        let kind = match n.kind.as_str() {
                            "prompt" => "提示词 文本",
                            "asset" => "参考 素材 图片",
                            "image_generate" => "图像生成 图片生成",
                            _ => "",
                        };
                        n.title.to_lowercase().contains(&q)
                            || n.id.to_lowercase().contains(&q)
                            || n.kind.contains(&q)
                            || kind.contains(&q)
                            || ["text", "prompt"].iter().any(|key| {
                                n.params[*key]
                                    .as_str()
                                    .is_some_and(|t| t.to_lowercase().contains(&q))
                            })
                    })
                    .collect();
                let index = if h.editor.search == q {
                    matches
                        .iter()
                        .position(|n| n.id == s.selected)
                        .map_or(0, |i| (i + 1) % matches.len().max(1))
                } else {
                    0
                };
                matches.get(index).map(|n| n.id.clone())
            });
            h.editor.search = q;
            let r = if let Some(id) = id {
                h.select_nodes(BTreeSet::from([id]), true)
            } else {
                Err("没有匹配的节点".into())
            };
            drop(h);
            finish(&app, &host, r.map(|()| None));
        }
    });
}
