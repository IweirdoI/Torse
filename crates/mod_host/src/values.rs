//! Identical bounded value conversion for distinct bindgen world types.

macro_rules! boundary_values {
    () => {
        fn encode_fields(
            fields: &torse_world::ComponentFields,
            symbol: impl Fn(&torse_types::ContentKey) -> Option<u32>,
        ) -> Option<Vec<Field>> {
            use torse::behavior::types::{NodeField, ValueNode as N, ValueTree};
            use torse_world::ComponentValue as V;
            torse_world::component_size(fields).ok()?;
            fn node(
                value: &V,
                nodes: &mut Vec<N>,
                symbol: &impl Fn(&torse_types::ContentKey) -> Option<u32>,
            ) -> Option<u32> {
                let value = match value {
                    V::Bool(v) => N::Bool(*v),
                    V::U32(v) => N::U32(*v),
                    V::I64(v) => N::I64(*v),
                    V::Content(v) => N::Content(symbol(v)?),
                    V::Entity(v) => N::Entity(torse::behavior::types::EntityId {
                        low: v.0 as u64,
                        high: (v.0 >> 64) as u64,
                    }),
                    V::Text(v) => N::Text(v.clone()),
                    V::List(values) => N::List(
                        values
                            .iter()
                            .map(|v| node(v, nodes, symbol))
                            .collect::<Option<_>>()?,
                    ),
                    V::Record(values) => N::Record(
                        values
                            .iter()
                            .map(|(k, v)| {
                                Some(NodeField {
                                    key: k.to_string(),
                                    value: node(v, nodes, symbol)?,
                                })
                            })
                            .collect::<Option<_>>()?,
                    ),
                };
                let index = nodes.len() as u32;
                nodes.push(value);
                Some(index)
            }
            fields
                .iter()
                .map(|(key, value)| {
                    let value = match value {
                        V::Bool(v) => Atom::Bool(*v),
                        V::U32(v) => Atom::U32(*v),
                        V::I64(v) => Atom::I64(*v),
                        V::Content(v) => Atom::Content(symbol(v)?),
                        V::Entity(v) => Atom::Entity(torse::behavior::types::EntityId {
                            low: v.0 as u64,
                            high: (v.0 >> 64) as u64,
                        }),
                        V::Text(v) => Atom::Text(v.clone()),
                        V::List(_) | V::Record(_) => {
                            let mut nodes = Vec::new();
                            node(value, &mut nodes, &symbol)?;
                            Atom::Tree(ValueTree { nodes })
                        }
                    };
                    Some(Field {
                        key: key.to_string(),
                        value,
                    })
                })
                .collect()
        }

        fn decode_fields(
            fields: &[Field],
            symbol: impl Fn(u32) -> Option<torse_types::ContentKey>,
        ) -> Result<torse_world::ComponentFields, String> {
            use torse::behavior::types::ValueNode as N;
            use torse_world::ComponentValue as V;
            let entity = |v: &torse::behavior::types::EntityId| {
                torse_types::EntityId((v.high as u128) << 64 | v.low as u128)
            };
            let content = |v| {
                symbol(v)
                    .map(V::Content)
                    .ok_or("unknown content symbol".to_string())
            };
            let mut result = torse_world::ComponentFields::new();
            let mut total_nodes = fields
                .iter()
                .filter(|f| !matches!(f.value, Atom::Tree(_)))
                .count();
            for field in fields {
                let value = match &field.value {
                    Atom::Bool(v) => V::Bool(*v),
                    Atom::U32(v) => V::U32(*v),
                    Atom::I64(v) => V::I64(*v),
                    Atom::Content(v) => content(*v)?,
                    Atom::Entity(v) => V::Entity(entity(v)),
                    Atom::Text(v) => V::Text(v.clone()),
                    Atom::Tree(tree) => {
                        total_nodes = total_nodes.saturating_add(tree.nodes.len());
                        if tree.nodes.is_empty() || total_nodes > torse_world::MAX_COMPONENT_NODES {
                            return Err("value node limit exceeded".into());
                        }
                        let mut values: Vec<Option<V>> = Vec::with_capacity(tree.nodes.len());
                        let mut depths: Vec<u32> = Vec::with_capacity(tree.nodes.len());
                        for (index, node) in tree.nodes.iter().enumerate() {
                            let mut depth = 0;
                            let mut child = |id: u32| -> Result<V, String> {
                                let id = id as usize;
                                if id >= index {
                                    return Err("invalid postorder value index".into());
                                }
                                depth = depth.max(depths[id].saturating_add(1));
                                if depth > torse_world::MAX_VALUE_DEPTH {
                                    return Err("value depth limit exceeded".into());
                                }
                                values[id].take().ok_or("shared value node".into())
                            };
                            let value = match node {
                                N::Bool(v) => V::Bool(*v),
                                N::U32(v) => V::U32(*v),
                                N::I64(v) => V::I64(*v),
                                N::Content(v) => content(*v)?,
                                N::Entity(v) => V::Entity(entity(v)),
                                N::Text(v) => V::Text(v.clone()),
                                N::List(ids) => V::List(
                                    ids.iter().map(|id| child(*id)).collect::<Result<_, _>>()?,
                                ),
                                N::Record(fields) => {
                                    let mut record = torse_world::ComponentFields::new();
                                    for f in fields {
                                        let key = torse_types::ContentKey::parse(&f.key)
                                            .map_err(|e| e.to_string())?;
                                        if record.insert(key, child(f.value)?).is_some() {
                                            return Err("duplicate record field".into());
                                        }
                                    }
                                    V::Record(record)
                                }
                            };
                            values.push(Some(value));
                            depths.push(depth);
                        }
                        let root = values.pop().flatten().ok_or("missing value root")?;
                        if values.iter().any(Option::is_some) {
                            return Err("unused value node".into());
                        }
                        root
                    }
                };
                let key = torse_types::ContentKey::parse(&field.key).map_err(|e| e.to_string())?;
                if result.insert(key, value).is_some() {
                    return Err("duplicate component field".into());
                }
            }
            torse_world::component_size(&result)
                .map_err(|e| format!("invalid component: {e:?}"))?;
            Ok(result)
        }

        #[cfg(test)]
        mod value_boundary_tests {
            use super::*;
            use torse::behavior::types::{ValueNode as N, ValueTree};
            fn field(nodes: Vec<N>) -> Field {
                Field {
                    key: "test:field/value".into(),
                    value: Atom::Tree(ValueTree { nodes }),
                }
            }
            #[test]
            fn malformed_graphs_cannot_expand_share_or_cycle() {
                for nodes in [
                    vec![],
                    vec![N::List(vec![0])],
                    vec![N::U32(1), N::List(vec![0, 0])],
                    vec![N::U32(1), N::U32(2)],
                ] {
                    assert!(decode_fields(&[field(nodes)], |_| None).is_err());
                }
                let mut nodes = vec![N::U32(1)];
                for index in 0..10 {
                    nodes.push(N::List(vec![index]));
                }
                assert!(decode_fields(&[field(nodes)], |_| None).is_err());
            }
            #[test]
            fn duplicate_keys_and_oversized_values_are_rejected() {
                let f = Field {
                    key: "test:field/value".into(),
                    value: Atom::U32(1),
                };
                assert!(decode_fields(&[f.clone(), f], |_| None).is_err());
                let f = field(vec![N::Text("x".repeat(65_536))]);
                assert!(decode_fields(&[f], |_| None).is_err());
            }
            #[test]
            fn native_nested_values_roundtrip_without_changing_order() {
                use torse_world::ComponentValue as V;
                let key = torse_types::ContentKey::parse("test:field/value").unwrap();
                let fields = [(
                    key.clone(),
                    V::List(vec![V::Record([(key, V::I64(-7))].into()), V::U32(9)]),
                )]
                .into();
                let encoded = encode_fields(&fields, |_| None).unwrap();
                assert_eq!(decode_fields(&encoded, |_| None).unwrap(), fields);
            }
            #[test]
            fn exact_native_node_limit_is_representable() {
                let mut nodes = vec![N::U32(1); torse_world::MAX_COMPONENT_NODES - 1];
                nodes.push(N::List((0..nodes.len() as u32).collect()));
                assert!(decode_fields(&[field(nodes)], |_| None).is_ok());
            }
        }
    };
}

pub(crate) use boundary_values;
