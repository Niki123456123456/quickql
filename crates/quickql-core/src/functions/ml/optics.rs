use linfa::prelude::Transformer;
use linfa_clustering::Optics;
use ndarray::Array2;
use quickql_macros::fn_info;
use serde_json::Value;

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct Config {
    min_points : Option<usize>,
    eps : Option<f64>,
    cluster_eps : Option<f64>,
}

#[fn_info()]
pub(crate) fn optics(rows: Vec<Vec<f64>>, config: Config) -> Value {
    if rows.len() < 2 {
        return Value::Null;
    }

    let n_features = rows[0].len();
    if n_features == 0 || rows.iter().any(|row| row.len() != n_features) {
        return Value::Null;
    }

    let min_points = config
        .min_points
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

    let eps = config
        .eps
        .unwrap_or(f64::MAX);
    if !eps.is_finite() || eps <= 0.0 {
        return Value::Null;
    }
    let cluster_eps = config
        .cluster_eps
        .unwrap_or(eps);
    if !cluster_eps.is_finite() || cluster_eps <= 0.0 {
        return Value::Null;
    }

    let params = Optics::params(min_points).tolerance(eps);

    let Ok(analysis) = params.transform(observations.view()) else {
        return Value::Null;
    };

    let mut current_cluster: Option<usize> = None;
    let mut next_cluster = 0;

    let mut samples: Vec<_> = analysis
        .iter()
        .map(|sample: &linfa_clustering::Sample<f64>| {
            let core_distance = *sample.core_distance();
            let reachability_distance = *sample.reachability_distance();
            let starts_cluster =
                core_distance.is_some_and(|distance| distance <= cluster_eps);
            let cluster_index = if reachability_distance
                .is_none_or(|distance| distance > cluster_eps)
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

            Sample {
                cluster_index,
                index: sample.index(),
                core_distance,
                reachability_distance,
            }
        })
        .collect();

    samples.sort_by_key(|sample| sample.index);

    serde_json::json!({
        "clusterIndex": samples.iter().map(|x|x.cluster_index).collect::<Vec<_>>(),
        "index": samples.iter().map(|x|x.index).collect::<Vec<_>>(),
        "embedding": samples.iter().map(|x|rows[x.index].clone()).collect::<Vec<_>>(),
        "coreDistance": samples.iter().map(|x|x.core_distance).collect::<Vec<_>>(),
        "reachabilityDistance": samples.iter().map(|x|x.reachability_distance).collect::<Vec<_>>(),
    })
}

struct Sample {
    cluster_index: Option<usize>,
    index: usize,
    core_distance: Option<f64>,
    reachability_distance: Option<f64>,
}
