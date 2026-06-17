use linfa::prelude::Transformer;
use linfa_clustering::Optics;
use ndarray::Array2;
use serde_json::Value;

use crate::umap::{get_f64, get_usize, parse_matrix};

pub(crate) fn optics_value(data: Option<&Value>, config: Option<&Value>) -> Value {
    let Some(rows) = data.and_then(parse_matrix) else {
        return Value::Null;
    };

    if rows.len() < 2 {
        return Value::Null;
    }

    let n_features = rows[0].len();
    if n_features == 0 || rows.iter().any(|row| row.len() != n_features) {
        return Value::Null;
    }

    let config = config.and_then(Value::as_object);
    let min_points = config
        .and_then(|config| get_usize(config, &["minPoints", "min_points"]))
        .unwrap_or(2);
    if min_points < 2 || min_points > rows.len() {
        return Value::Null;
    }

    let observations = match Array2::from_shape_vec(
        (rows.len(), n_features),
        rows.iter().flatten().copied().collect(),
    ) {
        Ok(observations) => observations,
        Err(_) => return Value::Null,
    };

    let tolerance = config
        .and_then(|config| get_f64(config, &["tolerance", "eps"]))
        .unwrap_or(f64::MAX);
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return Value::Null;
    }
    let cluster_tolerance = config
        .and_then(|config| {
            get_f64(
                config,
                &[
                    "clusterTolerance",
                    "cluster_tolerance",
                    "clusterEps",
                    "cluster_eps",
                ],
            )
        })
        .unwrap_or(tolerance);
    if !cluster_tolerance.is_finite() || cluster_tolerance <= 0.0 {
        return Value::Null;
    }

    let mut params = Optics::params(min_points);
    if config
        .and_then(|config| get_f64(config, &["tolerance", "eps"]))
        .is_some()
    {
        params = params.tolerance(tolerance);
    }

    let Ok(analysis) = params.transform(observations.view()) else {
        return Value::Null;
    };

    let mut current_cluster: Option<usize> = None;
    let mut next_cluster = 0;

    Value::Array(
        analysis
            .iter()
            .map(|sample| {
                let core_distance = *sample.core_distance();
                let reachability_distance = *sample.reachability_distance();
                let starts_cluster =
                    core_distance.is_some_and(|distance| distance <= cluster_tolerance);
                let cluster_index = if reachability_distance
                    .is_none_or(|distance| distance > cluster_tolerance)
                    || current_cluster.is_none()
                {
                    if starts_cluster {
                        let cluster_index = next_cluster;
                        next_cluster += 1;
                        current_cluster = Some(cluster_index);
                        Some(cluster_index)
                    } else {
                        current_cluster = None;
                        None
                    }
                } else {
                    current_cluster
                };

                serde_json::json!({
                    "clusterIndex": cluster_index,
                    "index": sample.index(),
                    "embedding": rows[sample.index()],
                    "coreDistance": core_distance,
                    "reachabilityDistance": reachability_distance,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optics_value_returns_ordered_sample_analysis() {
        let result = optics_value(
            Some(&serde_json::json!([[0.0, 0.0], [0.0, 0.1], [10.0, 10.0]])),
            Some(&serde_json::json!({"minPoints": 2, "tolerance": 20.0, "clusterTolerance": 1.0})),
        );

        let rows = result.as_array().expect("OPTICS should return sample rows");
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.get("index").is_some()));
        assert!(rows.iter().all(|row| row.get("coreDistance").is_some()));
        assert!(rows
            .iter()
            .all(|row| row.get("reachabilityDistance").is_some()));
        assert!(rows
            .iter()
            .any(|row| row["clusterIndex"].as_u64() == Some(0)));
        assert!(rows.iter().any(|row| row["clusterIndex"].is_null()));
    }

    #[test]
    fn optics_value_rejects_invalid_input() {
        assert_eq!(
            optics_value(Some(&serde_json::json!([[1.0]])), None),
            Value::Null
        );
        assert_eq!(
            optics_value(
                Some(&serde_json::json!([[1.0], [2.0]])),
                Some(&serde_json::json!({"minPoints": 3}))
            ),
            Value::Null
        );
        assert_eq!(
            optics_value(
                Some(&serde_json::json!([[1.0], [2.0]])),
                Some(&serde_json::json!({"clusterTolerance": 0.0}))
            ),
            Value::Null
        );
    }
}
