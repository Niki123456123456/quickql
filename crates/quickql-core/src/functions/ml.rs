use crate::{FnInfo, MetaParameters};
use linfa_nn::{
    distance::{Distance, L2Dist},
    CommonNearestNeighbour, NearestNeighbour,
};
use ndarray::{Array2, ArrayView, ArrayView1, Dimension};
use quickql_macros::fn_info;
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Mutex,
};
use std::thread;

pub(crate) fn infos() -> Vec<FnInfo> {
    vec![
        softmax_info(),
        entropy_info(),
        l2_info(),
        random_info(),
        nn2_info(),
        nn_info(),
        subtract_info(),
        crate::tsne::tsne_info(),
        crate::optics::optics_info(),
        crate::umap::umap_info(),
    ]
}

#[fn_info()]
fn softmax(values: &Vec<f64>) -> Vec<f64> {
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exp_values: Vec<_> = values.iter().map(|value| (value - max).exp()).collect();
    let sum: f64 = exp_values.iter().sum();

    exp_values.into_iter().map(|value| value / sum).collect()
}

#[fn_info()]
fn entropy(values: &Vec<f64>) -> f64 {
    let sum: f64 = values.iter().sum();

    let entropy: f64 = values
        .iter()
        .filter(|value| **value > 0.0)
        .map(|value| {
            let probability = value / sum;
            -probability * probability.log2()
        })
        .sum();

    entropy
}

#[fn_info()]
fn l2(values: &Vec<f64>) -> f64 {
    values.iter().map(|value| value * value).sum::<f64>().sqrt()
}

#[fn_info()]
fn random() -> f64 {
    rand::random()
}

#[fn_info()]
fn subtract(a: f64, b: f64) -> f64 {
    a - b
}

// k-nearest-neighbor
#[fn_info()]
fn nn2(rows: Vec<Vec<f64>>, neighbors: Vec<Vec<f64>>, k: usize) -> Value {
    if rows.is_empty() || neighbors.is_empty() || k == 0 {
        return Value::Null;
    }

    let n_features = rows[0].len();
    if n_features == 0
        || rows
            .iter()
            .any(|row| row.len() != n_features || row.iter().any(|value| !value.is_finite()))
        || neighbors
            .iter()
            .any(|row| row.len() != n_features || row.iter().any(|value| !value.is_finite()))
    {
        return Value::Null;
    }

    let neighbor_count = k.min(neighbors.len());
    let mut indices = Vec::with_capacity(rows.len());
    let mut distances = Vec::with_capacity(rows.len());

    for row in &rows {
        let mut matches = neighbors
            .iter()
            .enumerate()
            .map(|(index, neighbor)| (euclidean_distance(row, neighbor), index))
            .collect::<Vec<_>>();

        matches.sort_unstable_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        matches.truncate(neighbor_count);

        distances.push(
            matches
                .iter()
                .map(|(distance, _)| *distance)
                .collect::<Vec<_>>(),
        );
        indices.push(matches.iter().map(|(_, index)| *index).collect::<Vec<_>>());
    }

    serde_json::json!({
        "distance": distances,
        "index": indices,
    })
}

fn euclidean_distance(left: &[f64], right: &[f64]) -> f64 {
    left.iter()
        .zip(right)
        .fold(0.0, |distance, (left, right)| distance.hypot(left - right))
}

// k-nearest-neighbor
#[fn_info()]
fn nn(
    rows: Vec<Vec<f64>>,
    neighbors: Vec<Vec<f64>>,
    k: usize,
    distance: Option<&Value>,
    additional: Option<&Value>,
    params: MetaParameters,
) -> Value {
    if rows.is_empty() || neighbors.is_empty() || k == 0 {
        return Value::Null;
    }
    let additional = match additional {
        None | Some(Value::Null) => None,
        Some(additional) => match additional.as_array() {
            Some(additional) if additional.len() == neighbors.len() => Some(additional),
            _ => return Value::Null,
        },
    };
    let Some(distance) = DistanceMetric::parse(distance) else {
        return Value::Null;
    };

    let n_features = rows[0].len();
    if n_features == 0
        || rows
            .iter()
            .any(|row| row.len() != n_features || row.iter().any(|value| !value.is_finite()))
        || neighbors
            .iter()
            .any(|row| row.len() != n_features || row.iter().any(|value| !value.is_finite()))
    {
        return Value::Null;
    }

    let observations = match Array2::from_shape_vec(
        (neighbors.len(), n_features),
        neighbors.iter().flatten().copied().collect(),
    ) {
        Ok(observations) => observations,
        Err(_) => return Value::Null,
    };

    let queries = match Array2::from_shape_vec(
        (rows.len(), n_features),
        rows.iter().flatten().copied().collect(),
    ) {
        Ok(queries) => queries,
        Err(_) => return Value::Null,
    };

    let index = match distance {
        DistanceMetric::L2 => CommonNearestNeighbour::KdTree.from_batch(&observations, L2Dist),
        DistanceMetric::Cosine => {
            CommonNearestNeighbour::LinearSearch.from_batch(&observations, CosineDist)
        }
    };
    let Ok(index) = index else {
        return Value::Null;
    };

    let total = queries.nrows();
    let worker_count = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(total);
    let next_query = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let output = Mutex::new(vec![None; total]);
    let (completed_sender, completed_receiver) = mpsc::channel();

    let succeeded = thread::scope(|scope| {
        for _ in 0..worker_count {
            let completed_sender = completed_sender.clone();
            let next_query = &next_query;
            let failed = &failed;
            let output = &output;
            let index = &index;
            let queries = &queries;

            scope.spawn(move || loop {
                if failed.load(Ordering::Relaxed) {
                    break;
                }

                let query_index = next_query.fetch_add(1, Ordering::Relaxed);
                if query_index >= total {
                    break;
                }

                let query = queries.row(query_index);
                let Ok(matches) = index.k_nearest(query, k) else {
                    failed.store(true, Ordering::Relaxed);
                    let _ = completed_sender.send(false);
                    break;
                };
                let indices = matches
                    .iter()
                    .map(|(_, neighbor_index)| *neighbor_index)
                    .collect::<Vec<_>>();
                let distances = matches
                    .iter()
                    .map(|(neighbor, _)| distance.distance(query, *neighbor))
                    .collect::<Vec<_>>();

                output.lock().unwrap()[query_index] = Some((indices, distances));
                if completed_sender.send(true).is_err() {
                    break;
                }
            });
        }
        drop(completed_sender);

        for completed in 1..=total {
            match completed_receiver.recv() {
                Ok(true) => params.progress.report("NN", completed, total),
                Ok(false) | Err(_) => return false,
            }
        }
        true
    });

    if !succeeded {
        return Value::Null;
    }

    let (indices, distances): (Vec<_>, Vec<_>) = output
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|result| result.unwrap())
        .unzip();

    let mut result = serde_json::json!({
        "distance": distances,
        "index": indices,
    });
    if let Some(additional) = additional {
        let values = indices
            .iter()
            .map(|row_indices| {
                Value::Array(
                    row_indices
                        .iter()
                        .map(|index| additional[*index].clone())
                        .collect(),
                )
            })
            .collect();
        result["additional"] = Value::Array(values);
    }
    result
}

#[derive(Clone, Copy)]
enum DistanceMetric {
    L2,
    Cosine,
}

impl DistanceMetric {
    fn parse(value: Option<&Value>) -> Option<Self> {
        match value {
            None | Some(Value::Null) => Some(Self::L2),
            Some(Value::String(value)) if value.eq_ignore_ascii_case("l2") => Some(Self::L2),
            Some(Value::String(value)) if value.eq_ignore_ascii_case("euclidean") => Some(Self::L2),
            Some(Value::String(value)) if value.eq_ignore_ascii_case("cosine") => {
                Some(Self::Cosine)
            }
            _ => None,
        }
    }

    fn distance(self, left: ArrayView1<'_, f64>, right: ArrayView1<'_, f64>) -> f64 {
        match self {
            Self::L2 => L2Dist.distance(left, right),
            Self::Cosine => CosineDist.distance(left, right),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CosineDist;

impl Distance<f64> for CosineDist {
    fn distance<D: Dimension>(
        &self,
        left: ArrayView<'_, f64, D>,
        right: ArrayView<'_, f64, D>,
    ) -> f64 {
        let (dot_product, left_norm, right_norm) = left.iter().zip(right.iter()).fold(
            (0.0, 0.0, 0.0),
            |(dot_product, left_norm, right_norm), (left, right)| {
                (
                    dot_product + left * right,
                    left_norm + left * left,
                    right_norm + right * right,
                )
            },
        );

        if left_norm == 0.0 || right_norm == 0.0 {
            return 1.0;
        }

        1.0 - (dot_product / (left_norm.sqrt() * right_norm.sqrt())).clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{nn, nn2, random, DistanceMetric};
    use crate::{FsFileProvider, MetaParameters, NoopFunctionProgress};
    use serde_json::{json, Value};
    use std::path::Path;

    #[test]
    fn nn2_returns_nearest_neighbors_in_distance_order() {
        let result = nn2(
            vec![vec![0.0, 0.0], vec![10.0, 10.0]],
            vec![vec![3.0, 4.0], vec![0.0, 1.0], vec![10.0, 9.0]],
            2,
        );

        assert_eq!(result["index"], json!([[1, 0], [2, 0]]));
        assert_eq!(result["distance"][0], json!([1.0, 5.0]));
        assert_eq!(result["distance"][1][0], json!(1.0));
    }

    #[test]
    fn nn2_caps_k_and_breaks_distance_ties_by_index() {
        let result = nn2(vec![vec![0.0]], vec![vec![1.0], vec![-1.0], vec![2.0]], 10);

        assert_eq!(result["index"], json!([[0, 1, 2]]));
        assert_eq!(result["distance"], json!([[1.0, 1.0, 2.0]]));
    }

    #[test]
    fn nn2_rejects_invalid_input() {
        assert_eq!(nn2(Vec::new(), vec![vec![1.0]], 1), Value::Null);
        assert_eq!(nn2(vec![vec![1.0]], vec![vec![1.0]], 0), Value::Null);
        assert_eq!(nn2(vec![vec![1.0, 2.0]], vec![vec![1.0]], 1), Value::Null);
        assert_eq!(nn2(vec![vec![f64::NAN]], vec![vec![1.0]], 1), Value::Null);
    }

    #[test]
    fn parallel_nn_preserves_query_order() {
        let rows = (0..64).map(|value| vec![value as f64]).collect::<Vec<_>>();
        let neighbors = rows.clone();
        let expected_indices = (0..64).map(|value| json!([value])).collect::<Vec<_>>();
        let mut ql_stack = Vec::new();
        let mut progress = NoopFunctionProgress;

        let result = nn(
            rows,
            neighbors,
            1,
            None,
            None,
            MetaParameters {
                query_path: Path::new("query.ql"),
                ql_stack: &mut ql_stack,
                file_provider: &FsFileProvider,
                progress: &mut progress,
            },
        );

        assert_eq!(result["index"], Value::Array(expected_indices));
        assert_eq!(result["distance"], json!(vec![vec![0.0]; 64]));
    }

    #[test]
    fn nn_includes_additional_values_for_each_neighbor() {
        let mut ql_stack = Vec::new();
        let mut progress = NoopFunctionProgress;

        let result = nn(
            vec![vec![0.0], vec![9.0]],
            vec![vec![2.0], vec![0.0], vec![10.0]],
            2,
            None,
            Some(&json!(["two", "zero", "ten"])),
            MetaParameters {
                query_path: Path::new("query.ql"),
                ql_stack: &mut ql_stack,
                file_provider: &FsFileProvider,
                progress: &mut progress,
            },
        );

        assert_eq!(result["index"], json!([[1, 0], [2, 0]]));
        assert_eq!(
            result["additional"],
            json!([["zero", "two"], ["ten", "two"]])
        );
    }

    #[test]
    fn nn_rejects_additional_values_with_a_different_length() {
        let mut ql_stack = Vec::new();
        let mut progress = NoopFunctionProgress;

        assert_eq!(
            nn(
                vec![vec![0.0]],
                vec![vec![0.0], vec![1.0]],
                1,
                None,
                Some(&json!(["only one"])),
                MetaParameters {
                    query_path: Path::new("query.ql"),
                    ql_stack: &mut ql_stack,
                    file_provider: &FsFileProvider,
                    progress: &mut progress,
                },
            ),
            Value::Null
        );
    }

    #[test]
    fn random_returns_a_value_between_zero_and_one() {
        for _ in 0..100 {
            let value = random();
            assert!((0.0..1.0).contains(&value));
        }
    }

    #[test]
    fn distance_metric_accepts_supported_names_and_rejects_unknown_names() {
        assert!(matches!(
            DistanceMetric::parse(None),
            Some(DistanceMetric::L2)
        ));
        assert!(matches!(
            DistanceMetric::parse(Some(&json!("Euclidean"))),
            Some(DistanceMetric::L2)
        ));
        assert!(matches!(
            DistanceMetric::parse(Some(&json!("COSINE"))),
            Some(DistanceMetric::Cosine)
        ));
        assert!(DistanceMetric::parse(Some(&json!("manhattan"))).is_none());
    }
}
