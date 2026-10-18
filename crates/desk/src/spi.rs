//! The Linux desk backend: the AT-SPI client's async calls, run on the worker thread by a runtime of its own so their fan-out stays concurrent.

use tokio::runtime::{Builder, Runtime};

use crate::atspi::{self, Atspi, Obj};
use crate::model::{Access, Act, Extras, Frame, Node, Tree};

/// The AT-SPI client and the single-thread runtime its calls run on.
pub struct Spi {
    rt: Runtime,
    atspi: Atspi,
}

impl Spi {
    pub fn open() -> Result<Self, String> {
        let rt = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("start the AT-SPI runtime: {e}"))?;
        Ok(Self {
            rt,
            atspi: Atspi::new(),
        })
    }

    fn listed(&self) -> Result<Vec<atspi::Frame>, String> {
        let listed = self.rt.block_on(self.atspi.frames());
        if listed.is_err() {
            self.rt.block_on(self.atspi.reset());
        }
        listed
    }
}

impl Access for Spi {
    fn frames(&mut self) -> Result<Vec<Frame>, String> {
        Ok(self.listed()?.iter().map(frame).collect())
    }

    fn tree(&mut self, frame: &Frame, extras: Extras) -> Result<Tree, String> {
        let window = atspi::Frame {
            obj: Obj::parse(&frame.id)?,
            role: 0,
            name: frame.title.clone(),
            states: 0,
            pid: frame.pid,
        };
        let t = self
            .rt
            .block_on(self.atspi.tree(window, spi_extras(extras)))?;
        Ok(Tree {
            frame: frame.clone(),
            nodes: t.nodes.iter().map(node).collect(),
            truncated: t.truncated,
            phases: t.phases,
        })
    }

    fn node(&mut self, id: &str, extras: Extras, chars: usize) -> Result<Node, String> {
        let obj = Obj::parse(id)?;
        let chars = i32::try_from(chars).unwrap_or(i32::MAX);
        let n = self
            .rt
            .block_on(self.atspi.node(&obj, spi_extras(extras), chars))?;
        Ok(node(&n))
    }

    fn ancestors(&mut self, id: &str) -> Result<Vec<String>, String> {
        let obj = Obj::parse(id)?;
        let chain = self.rt.block_on(self.atspi.ancestors(&obj))?;
        Ok(chain.iter().map(Obj::id).collect())
    }

    fn at(&mut self, window: &str, x: i32, y: i32) -> Result<Option<String>, String> {
        let within = Obj::parse(window)?;
        let hit = self.rt.block_on(self.atspi.at_point(&within, x, y))?;
        Ok(hit.as_ref().map(Obj::id))
    }

    fn pid_of(&mut self, id: &str) -> Result<Option<u32>, String> {
        let obj = Obj::parse(id)?;
        Ok(self.rt.block_on(self.atspi.pid(&obj.bus)).ok())
    }

    fn act(&mut self, id: &str, act: Act) -> Result<bool, String> {
        let obj = Obj::parse(id)?;
        self.rt.block_on(async {
            match act {
                Act::Action => self.atspi.do_action(&obj).await,
                Act::Select => self.atspi.select(&obj).await,
                Act::Focus => self.atspi.grab_focus(&obj).await,
            }
        })
    }

    fn close(&mut self, frame: &Frame) -> Result<(), String> {
        Err(format!(
            "closing \"{}\" by its window manager is not in the Linux desk yet",
            frame.title
        ))
    }
}

fn frame(f: &atspi::Frame) -> Frame {
    Frame {
        id: f.obj.id(),
        title: f.name.clone(),
        role: atspi::role_name(f.role, true),
        pid: f.pid,
        active: f.active(),
        app: None,
        stale: false,
    }
}

fn node(n: &atspi::Node) -> Node {
    let (mnemonic, shortcut) = n.keys.as_deref().map_or((None, None), atspi::key_combos);
    Node {
        id: n.obj.as_ref().map(Obj::id).unwrap_or_default(),
        parent: n.parent,
        depth: n.depth,
        role: n.role(),
        native_role: n.native_role().map(str::to_owned),
        name: n.name.clone(),
        description: n.description.clone(),
        states: n.state_names(),
        value: n.value.clone(),
        shortcut,
        mnemonic,
        keys: n.keys.clone(),
        bounds: n.bounds,
    }
}

fn spi_extras(e: Extras) -> atspi::Extras {
    atspi::Extras {
        values: e.values,
        bounds: e.bounds,
        keys: e.keys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_keeps_the_atspi_states_role_and_raw_key_binding() {
        let n = atspi::Node {
            obj: Some(Obj::parse("1.17/5").unwrap()),
            role: 43,
            name: "Save".into(),
            states: (1 << 8) | (1 << 25) | (1 << 30),
            ifaces: 0,
            parent: Some(0),
            depth: 2,
            keys: Some("<Alt>s;;<Control>s".into()),
            bounds: Some([1, 2, 3, 4]),
            ..atspi::Node::default()
        };
        let m = node(&n);
        assert_eq!(m.id, "1.17/5");
        assert_eq!(m.role, "button");
        assert_eq!(m.states, vec!["showing", "visible"]);
        assert_eq!(m.mnemonic.as_deref(), Some("alt+s"));
        assert_eq!(m.shortcut.as_deref(), Some("ctrl+s"));
        assert_eq!(m.keys.as_deref(), Some("<Alt>s;;<Control>s"));
        assert_eq!(m.bounds, Some([1, 2, 3, 4]));
    }

    #[test]
    fn a_frame_takes_the_window_role_its_id_and_active_state() {
        let f = atspi::Frame {
            obj: Obj::parse("1.17/5").unwrap(),
            role: 23,
            name: "Untitled - Mousepad".into(),
            states: 1 << 1,
            pid: Some(42),
        };
        let m = frame(&f);
        assert_eq!(
            (m.id.as_str(), m.title.as_str(), m.role, m.pid, m.active),
            ("1.17/5", "Untitled - Mousepad", "window", Some(42), true)
        );
    }
}
