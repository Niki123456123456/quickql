use faer::Mat;
use manifolds_rs::prelude::{NearestNeighbourParams, UmapGraphParams, UmapOptimParams};
use manifolds_rs::UmapParams;
use quickql_macros::fn_info;
use serde_json::Value;

#[fn_info()]
pub(crate) fn umap(rows: Vec<Vec<f64>>, config: Option<&Value>) -> Option<Vec<Vec<f64>>> {
    if rows.len() < 2 {
        return None;
    }

    let n_features = rows[0].len();
    if n_features == 0 || rows.iter().any(|row| row.len() != n_features) {
        return None;
    }

    let data = Mat::from_fn(rows.len(), n_features, |i, j| rows[i][j]);
    let config = config.and_then(Value::as_object);
    let params = params_from_config(config, rows.len());
    let seed = config
        .and_then(|config| get_usize(config, &["seed"]))
        .unwrap_or(42);
    let verbose = config
        .and_then(|config| get_usize(config, &["verbose"]))
        .unwrap_or(0);

    let Ok(embedding) = manifolds_rs::umap(data.as_ref(), None, &params, seed, verbose) else {
        return None;
    };

    Some(embedding)
}

fn params_from_config(
    config: Option<&serde_json::Map<String, Value>>,
    n_samples: usize,
) -> UmapParams<f64> {
    let min_dist = config
        .and_then(|config| get_f64(config, &["minDist", "min_dist"]))
        .unwrap_or(0.5);
    let spread = config
        .and_then(|config| get_f64(config, &["spread"]))
        .unwrap_or(1.0);

    let mut params = UmapParams::new_default_2d(Some(min_dist), Some(spread));
    params.k = config
        .and_then(|config| get_usize(config, &["k", "nNeighbors", "n_neighbors"]))
        .unwrap_or(params.k)
        .min(n_samples.saturating_sub(1))
        .max(1);

    if let Some(config) = config {
        params.n_dim = get_usize(config, &["nDim", "n_dim", "dimensions"]).unwrap_or(params.n_dim);
        params.optimiser =
            get_string(config, &["optimiser", "optimizer"]).unwrap_or(params.optimiser);
        params.ann_type = get_string(config, &["annType", "ann_type"]).unwrap_or(params.ann_type);
        params.initialisation = get_string(config, &["initialisation", "initialization"])
            .unwrap_or(params.initialisation);
        params.init_range = get_f64(config, &["initRange", "init_range"]);
        params.randomised =
            get_bool(config, &["randomised", "randomized"]).unwrap_or(params.randomised);

        params.nn_params = NearestNeighbourParams::default();
        params.umap_graph_params = UmapGraphParams {
            bandwidth: get_f64(config, &["bandwidth"])
                .unwrap_or(params.umap_graph_params.bandwidth),
            local_connectivity: get_f64(config, &["localConnectivity", "local_connectivity"])
                .unwrap_or(params.umap_graph_params.local_connectivity),
            mix_weight: get_f64(config, &["mixWeight", "mix_weight"])
                .unwrap_or(params.umap_graph_params.mix_weight),
        };
        params.optim_params = UmapOptimParams::from_min_dist_spread(
            min_dist,
            spread,
            get_f64(config, &["lr", "learningRate", "learning_rate"]),
            get_f64(config, &["gamma"]),
            get_usize(config, &["nEpochs", "n_epochs"]),
            get_usize(
                config,
                &["negSampleRate", "neg_sample_rate", "negativeSampleRate"],
            ),
            get_f64(config, &["beta1"]),
            get_f64(config, &["beta2"]),
            get_f64(config, &["eps"]),
        );
    }

    params
}

pub(crate) fn get_string(config: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| config.get(*key).and_then(Value::as_str))
        .map(ToString::to_string)
}

pub(crate) fn get_usize(config: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<usize> {
    keys.iter()
        .find_map(|key| config.get(*key).and_then(Value::as_u64))
        .and_then(|value| usize::try_from(value).ok())
}

pub(crate) fn get_f64(config: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| config.get(*key).and_then(Value::as_f64))
}

pub(crate) fn get_bool(config: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| config.get(*key).and_then(Value::as_bool))
}
