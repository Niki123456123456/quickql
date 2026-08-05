use crate::FnInfo;
use linfa_nn::{distance::L2Dist, CommonNearestNeighbour, NearestNeighbour};
use ndarray::{Array2, ArrayView1};
use quickql_macros::fn_info;
use serde_json::Value;

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
fn subtract(a: f64, b : f64)-> f64{
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
fn nn(rows: Vec<Vec<f64>>, neighbors: Vec<Vec<f64>>, k: usize) -> Value {
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

    let Ok(index) = CommonNearestNeighbour::KdTree.from_batch(&observations, L2Dist) else {
        return Value::Null;
    };

    let mut indices = Vec::with_capacity(rows.len());
    let mut distances = Vec::with_capacity(rows.len());

    for query in queries.rows() {
        let Ok(matches) = index.k_nearest(query, k) else {
            return Value::Null;
        };

        indices.push(
            matches
                .iter()
                .map(|(_, neighbor_index)| *neighbor_index)
                .collect::<Vec<_>>(),
        );
        distances.push(
            matches
                .iter()
                .map(|(neighbor, _)| l2_distance(query, *neighbor))
                .collect::<Vec<_>>(),
        );
    }

    serde_json::json!({
        "distance": distances,
        "index": indices,
    })
}

fn l2_distance(left: ArrayView1<'_, f64>, right: ArrayView1<'_, f64>) -> f64 {
    left.iter()
        .zip(right.iter())
        .map(|(left, right)| {
            let difference = left - right;
            difference * difference
        })
        .sum::<f64>()
        .sqrt()
}

#[cfg(test)]
mod tests {
    use super::{nn2, random};
    use serde_json::{json, Value};

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
    fn random_returns_a_value_between_zero_and_one() {
        for _ in 0..100 {
            let value = random();
            assert!((0.0..1.0).contains(&value));
        }
    }
}
