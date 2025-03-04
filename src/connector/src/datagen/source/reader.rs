// Copyright 2022 Singularity Data
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;

use super::field_generator::FieldGeneratorImpl;
use super::generator::DatagenEventGenerator;
use crate::datagen::source::SEQUENCE_FIELD_KIND;
use crate::datagen::{DatagenProperties, DatagenSplit};
use crate::{
    Column, ConnectorState, DataType, SourceMessage, SplitImpl, SplitMetaData, SplitReader,
};

const KAFKA_MAX_FETCH_MESSAGES: usize = 1024;

pub struct DatagenSplitReader {
    pub generator: DatagenEventGenerator,
    pub assigned_split: DatagenSplit,
}

#[async_trait]
impl SplitReader for DatagenSplitReader {
    type Properties = DatagenProperties;

    async fn new(
        properties: DatagenProperties,
        state: ConnectorState,
        columns: Option<Vec<Column>>,
    ) -> Result<Self>
    where
        Self: Sized,
    {
        let mut assigned_split = DatagenSplit::default();
        let mut split_id = String::new();
        let mut events_so_far = u64::default();
        if let Some(splits) = state {
            log::debug!("Splits for datagen found! {:?}", splits);
            for split in splits {
                // TODO: currently, assume there's only on split in one reader
                split_id = split.id();
                if let SplitImpl::Datagen(n) = split {
                    if let Some(s) = n.start_offset {
                        // start_offset in `SplitImpl` indicates the latest successfully generated
                        // index, so here we use start_offset+1
                        events_so_far = s + 1;
                    };
                    assigned_split = n;
                    break;
                }
            }
        }

        let split_index = assigned_split.split_index as u64;
        let split_num = assigned_split.split_num as u64;

        let rows_per_second = properties.rows_per_second.parse::<u64>()?;
        let fields_option_map = properties.fields;
        let mut fields_map = HashMap::<String, FieldGeneratorImpl>::new();

        // check columns
        assert!(columns.as_ref().is_some());
        let columns = columns.unwrap();
        assert!(columns.len() > 1);
        let columns = &columns[1..];

        // parse field connector option to build FieldGeneratorImpl
        // for example:
        // create materialized source s1  (
        //     f_sequence INT,
        //     f_random INT,
        //    ) with (
        //     'connector' = 'datagen',
        // 'fields.f_sequence.kind'='sequence',
        // 'fields.f_sequence.start'='1',
        // 'fields.f_sequence.end'='1000',

        // 'fields.f_random.min'='1',
        // 'fields.f_random.max'='1000',
        // 'fields.f_random.seed'='12345',

        // 'fields.f_random_str.length'='10'
        // )

        for column in columns {
            let name = column.name.clone();
            let kind_key = format!("fields.{}.kind", name);
            let data_type = column.data_type.clone();
            let random_seed_key = format!("fields.{}.seed", name);
            let random_seed: u64 = match fields_option_map
                .get(&random_seed_key)
                .map(|s| s.to_string())
            {
                Some(seed) => {
                    match seed.parse::<u64>() {
                        // we use given seed xor split_index to make sure every split has different
                        // seed
                        Ok(seed) => seed ^ split_index,
                        Err(e) => {
                            log::warn!("cannot parse {:?} to u64 due to {:?}, will use {:?} as random seed", seed, e, split_index);
                            split_index
                        }
                    }
                }
                None => split_index,
            };
            match column.data_type {
                DataType::Timestamp => {
                let max_past_key = format!("fields.{}.max_past", name);
                let max_past_value =
                fields_option_map.get(&max_past_key).map(|s| s.to_string());
                fields_map.insert(
                    name,
                    FieldGeneratorImpl::with_random(
                        data_type,
                            None,
                        None,
                        max_past_value,
                        None,
                        random_seed
                    )?,
                );},
                DataType::Varchar => {
                let length_key = format!("fields.{}.length", name);
                let length_value =
                fields_option_map.get(&length_key).map(|s| s.to_string());
                fields_map.insert(
                    name,
                    FieldGeneratorImpl::with_random(
                        data_type,
                        None,
                        None,
                        None,
                        length_value,
                        random_seed
                    )?,
                );},
                _ => {
                    if let Some(kind) = fields_option_map.get(&kind_key) && kind.as_str() == SEQUENCE_FIELD_KIND{
                        let start_key = format!("fields.{}.start", name);
                        let end_key = format!("fields.{}.end", name);
                        let start_value =
                            fields_option_map.get(&start_key).map(|s| s.to_string());
                        let end_value = fields_option_map.get(&end_key).map(|s| s.to_string());
                        fields_map.insert(
                            name,
                            FieldGeneratorImpl::with_sequence(
                                data_type,
                                start_value,
                                end_value,
                                split_index,
                                split_num
                            )?,
                        );
                    } else{
                        let min_key = format!("fields.{}.min", name);
                        let max_key = format!("fields.{}.max", name);
                        let min_value = fields_option_map.get(&min_key).map(|s| s.to_string());
                        let max_value = fields_option_map.get(&max_key).map(|s| s.to_string());
                        fields_map.insert(
                            name,
                            FieldGeneratorImpl::with_random(
                                data_type,
                                min_value,
                                max_value,
                                None,
                                None,
                                random_seed
                            )?,
                        );
                    }
                }
            }
        }

        let generator = DatagenEventGenerator::new(
            fields_map,
            rows_per_second,
            events_so_far,
            split_id,
            split_num,
            split_index,
        )?;

        Ok(DatagenSplitReader {
            generator,
            assigned_split,
        })
    }

    async fn next(&mut self) -> Result<Option<Vec<SourceMessage>>> {
        self.generator.next().await
    }
}

#[cfg(test)]
mod tests {
    use maplit::hashmap;

    use super::*;

    #[tokio::test]
    async fn test_generator() -> Result<()> {
        let mock_datum = vec![
            Column {
                name: "_".to_string(),
                data_type: DataType::Int64,
            },
            Column {
                name: "random_int".to_string(),
                data_type: DataType::Int32,
            },
            Column {
                name: "random_float".to_string(),
                data_type: DataType::Float32,
            },
            Column {
                name: "sequence_int".to_string(),
                data_type: DataType::Int32,
            },
        ];
        let state = Some(vec![SplitImpl::Datagen(DatagenSplit {
            split_index: 0,
            split_num: 1,
            start_offset: None,
        })]);
        let properties = DatagenProperties {
            split_num: None,
            rows_per_second: "10".to_string(),
            fields: hashmap! {
                "fields.random_int.min".to_string() => "1".to_string(),
                "fields.random_int.max".to_string() => "1000".to_string(),
                "fields.random_int.seed".to_string() => "12345".to_string(),

                "fields.random_float.min".to_string() => "1".to_string(),
                "fields.random_float.max".to_string() => "1000".to_string(),
                "fields.random_float.seed".to_string() => "12345".to_string(),

                "fields.sequence_int.kind".to_string() => "sequence".to_string(),
                "fields.sequence_int.start".to_string() => "1".to_string(),
                "fields.sequence_int.end".to_string() => "1000".to_string(),
            },
        };

        let mut reader = DatagenSplitReader::new(properties, state, Some(mock_datum)).await?;
        let res = b"{\"random_float\":533.1488647460938,\"random_int\":533,\"sequence_int\":1}";

        assert_eq!(
            res,
            reader.next().await.unwrap().unwrap()[0]
                .payload
                .as_ref()
                .unwrap()
                .as_ref()
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_random_deterministic() -> Result<()> {
        let mock_datum = vec![
            Column {
                name: "_".to_string(),
                data_type: DataType::Int64,
            },
            Column {
                name: "random_int".to_string(),
                data_type: DataType::Int32,
            },
        ];
        let state = Some(vec![SplitImpl::Datagen(DatagenSplit {
            split_index: 0,
            split_num: 1,
            start_offset: None,
        })]);
        let properties = DatagenProperties {
            split_num: None,
            rows_per_second: "10".to_string(),
            fields: HashMap::new(),
        };
        let mut reader =
            DatagenSplitReader::new(properties.clone(), state, Some(mock_datum.clone())).await?;
        let _ = reader.next().await;
        let v1 = reader.next().await?.unwrap();

        let state = Some(vec![SplitImpl::Datagen(DatagenSplit {
            split_index: 0,
            split_num: 1,
            start_offset: Some(9),
        })]);
        let mut reader = DatagenSplitReader::new(properties, state, Some(mock_datum)).await?;
        let v2 = reader.next().await?.unwrap();

        assert_eq!(v1, v2);
        Ok(())
    }
}
