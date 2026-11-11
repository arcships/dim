use super::super::sqlite::error;
use dim_agent_runtime::Result;
use rusqlite::{Connection, params};
use tokio_util::sync::CancellationToken;

struct Node {
    sequence: i64,
    left: Option<i64>,
    right: Option<i64>,
    height: i64,
}

pub(super) struct Tree<'a> {
    pub connection: &'a Connection,
    pub session: &'a str,
    #[cfg(feature = "testing")]
    pub statistics: Option<&'a super::super::query::Statistics>,
}

impl Tree<'_> {
    fn load(&self, id: i64) -> Result<Node> {
        let sql = "SELECT sequence,left_id,right_id,height FROM timeline_member_nodes WHERE session_id=?1 AND id=?2";
        let mut statement = self.connection.prepare_cached(sql).map_err(error)?;
        #[cfg(feature = "testing")]
        if self.statistics.is_some() {
            for status in [
                rusqlite::StatementStatus::VmStep,
                rusqlite::StatementStatus::FullscanStep,
                rusqlite::StatementStatus::Sort,
                rusqlite::StatementStatus::AutoIndex,
            ] {
                statement.reset_status(status);
            }
        }
        let node = statement
            .query_row(params![self.session, id], |row| {
                Ok(Node {
                    sequence: row.get(0)?,
                    left: row.get(1)?,
                    right: row.get(2)?,
                    height: row.get(3)?,
                })
            })
            .map_err(error)?;
        #[cfg(feature = "testing")]
        if let Some(statistics) = self.statistics {
            super::super::query::record_statement(
                statistics,
                self.connection,
                &statement,
                sql,
                &[self.session.to_owned().into(), id.into()],
                "member_node",
            )?;
        }
        if node.sequence <= 0
            || !(1..=64).contains(&node.height)
            || node.left.is_some_and(|child| child >= id || child <= 0)
            || node.right.is_some_and(|child| child >= id || child <= 0)
        {
            return Err(error("invalid member tree node"));
        }
        Ok(node)
    }

    fn height(&self, id: Option<i64>) -> Result<i64> {
        id.map_or(Ok(0), |id| self.load(id).map(|node| node.height))
    }

    fn save(&self, node: &Node) -> Result<i64> {
        let height = 1 + self.height(node.left)?.max(self.height(node.right)?);
        self.connection
            .prepare_cached("INSERT INTO timeline_member_nodes(session_id,sequence,left_id,right_id,height) VALUES(?1,?2,?3,?4,?5)")
            .map_err(error)?
            .execute(params![self.session, node.sequence, node.left, node.right, height])
            .map_err(error)?;
        Ok(self.connection.last_insert_rowid())
    }

    fn rotate_left(&self, mut node: Node) -> Result<i64> {
        let mut right = self.load(
            node.right
                .ok_or_else(|| error("member tree has no right child"))?,
        )?;
        node.right = right.left;
        right.left = Some(self.save(&node)?);
        self.save(&right)
    }

    fn rotate_right(&self, mut node: Node) -> Result<i64> {
        let mut left = self.load(
            node.left
                .ok_or_else(|| error("member tree has no left child"))?,
        )?;
        node.left = left.right;
        left.right = Some(self.save(&node)?);
        self.save(&left)
    }

    fn balance(&self, mut node: Node) -> Result<i64> {
        let balance = self.height(node.left)? - self.height(node.right)?;
        if balance > 1 {
            let left = self.load(node.left.expect("a left-heavy node has a left child"))?;
            if self.height(left.left)? < self.height(left.right)? {
                node.left = Some(self.rotate_left(left)?);
            }
            self.rotate_right(node)
        } else if balance < -1 {
            let right = self.load(node.right.expect("a right-heavy node has a right child"))?;
            if self.height(right.right)? < self.height(right.left)? {
                node.right = Some(self.rotate_right(right)?);
            }
            self.rotate_left(node)
        } else {
            self.save(&node)
        }
    }

    pub fn insert(&self, root: Option<i64>, sequence: i64) -> Result<i64> {
        let Some(root) = root else {
            return self.save(&Node {
                sequence,
                left: None,
                right: None,
                height: 1,
            });
        };
        let mut node = self.load(root)?;
        match sequence.cmp(&node.sequence) {
            std::cmp::Ordering::Less => node.left = Some(self.insert(node.left, sequence)?),
            std::cmp::Ordering::Greater => node.right = Some(self.insert(node.right, sequence)?),
            std::cmp::Ordering::Equal => return Ok(root),
        }
        self.balance(node)
    }

    pub fn remove(&self, root: Option<i64>, sequence: i64) -> Result<Option<i64>> {
        let Some(root) = root else { return Ok(None) };
        let mut node = self.load(root)?;
        match sequence.cmp(&node.sequence) {
            std::cmp::Ordering::Less => node.left = self.remove(node.left, sequence)?,
            std::cmp::Ordering::Greater => node.right = self.remove(node.right, sequence)?,
            std::cmp::Ordering::Equal => match (node.left, node.right) {
                (None, right) => return Ok(right),
                (left, None) => return Ok(left),
                (_, Some(right)) => {
                    let mut successor = self.load(right)?;
                    while let Some(left) = successor.left {
                        successor = self.load(left)?;
                    }
                    node.sequence = successor.sequence;
                    node.right = self.remove(node.right, node.sequence)?;
                }
            },
        }
        self.balance(node).map(Some)
    }

    pub fn page(
        &self,
        root: Option<i64>,
        low: i64,
        high: i64,
        descending: bool,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<i64>> {
        struct Page<'a> {
            tree: &'a Tree<'a>,
            low: i64,
            high: i64,
            descending: bool,
            limit: usize,
            cancel: &'a CancellationToken,
            members: Vec<i64>,
        }
        impl Page<'_> {
            fn visit(&mut self, root: Option<i64>) -> Result<()> {
                if self.members.len() == self.limit || self.low > self.high {
                    return Ok(());
                }
                let Some(root) = root else { return Ok(()) };
                super::super::query::cancelled(self.cancel)?;
                let node = self.tree.load(root)?;
                if node.sequence < self.low {
                    return self.visit(node.right);
                }
                if node.sequence > self.high {
                    return self.visit(node.left);
                }
                let (first, last) = if self.descending {
                    (node.right, node.left)
                } else {
                    (node.left, node.right)
                };
                self.visit(first)?;
                if self.members.len() < self.limit {
                    self.members.push(node.sequence);
                }
                self.visit(last)
            }
        }
        let mut page = Page {
            tree: self,
            low,
            high,
            descending,
            limit,
            cancel,
            members: Vec::new(),
        };
        page.visit(root)?;
        Ok(page.members)
    }
}
