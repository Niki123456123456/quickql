use faer::Mat;
use manifolds_rs::prelude::{NearestNeighbourParams, TsneOptimParams};
use manifolds_rs::TsneParams;
use quickql_macros::fn_info;
use serde_json::Value;

use crate::umap::{get_bool, get_f64, get_string, get_usize};

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct Config {
    approx: Option<String>,
    seed: Option<usize>,
    verbose: Option<usize>,
    perplexity: Option<f64>,
    n_dim: Option<usize>,
}

#[fn_info()]
pub(crate) fn tsne(rows: Vec<Vec<f64>>, config: Config) -> Option<Vec<Vec<f64>>> {
    if rows.len() < 2 {
        return None;
    }

    let n_features = rows[0].len();
    if n_features == 0 || rows.iter().any(|row| row.len() != n_features) {
        return None;
    }

    let data = Mat::from_fn(rows.len(), n_features, |i, j| rows[i][j]);
    let approx_type = config.approx.unwrap_or_else(|| "bh".to_string());
    let seed = config.seed.unwrap_or(42);
    let verbose = config.verbose.unwrap_or(0);

    let mut params = TsneParams::new_default_2d(config.perplexity);
    params.n_dim = config.n_dim.unwrap_or(params.n_dim);

    let Ok(embedding) =
        manifolds_rs::tsne(data.as_ref(), None, &params, &approx_type, seed, verbose)
    else {
        return None;
    };

    let n_dim = embedding.len();
    Some(
        (0..rows.len())
            .map(|row| {
                (0..n_dim)
                    .map(|dimension| embedding[dimension][row])
                    .collect()
            })
            .collect(),
    )
}

fn params_from_config(config: Option<&serde_json::Map<String, Value>>) -> TsneParams<f64> {
    let perplexity = config
        .and_then(|config| get_f64(config, &["perplexity"]))
        .filter(|perplexity| perplexity.is_finite() && *perplexity > 0.0);
    let mut params = TsneParams::new_default_2d(perplexity);

    if let Some(config) = config {
        params.n_dim = get_usize(config, &["nDim", "n_dim", "dimensions"]).unwrap_or(params.n_dim);
        params.ann_type = get_string(config, &["annType", "ann_type"]).unwrap_or(params.ann_type);
        params.initialisation = get_string(config, &["initialisation", "initialization"])
            .unwrap_or(params.initialisation);
        params.init_range = get_f64(config, &["initRange", "init_range"])
            .filter(|value| value.is_finite() && *value > 0.0);
        params.randomised_init = get_bool(
            config,
            &[
                "randomisedInit",
                "randomizedInit",
                "randomised_init",
                "randomized_init",
                "randomised",
                "randomized",
            ],
        )
        .unwrap_or(params.randomised_init);

        params.nn_params = NearestNeighbourParams::default();
        params.optim_params = TsneOptimParams::new(
            get_usize(config, &["nEpochs", "n_epochs"]).unwrap_or(params.optim_params.n_epochs),
            get_f64(config, &["lr", "learningRate", "learning_rate"])
                .filter(|value| value.is_finite() && *value > 0.0),
            get_usize(
                config,
                &[
                    "earlyExagIter",
                    "early_exag_iter",
                    "earlyExaggerationIter",
                    "early_exaggeration_iter",
                ],
            )
            .unwrap_or(params.optim_params.early_exag_iter),
            get_f64(
                config,
                &[
                    "earlyExagFactor",
                    "early_exag_factor",
                    "earlyExaggerationFactor",
                    "early_exaggeration_factor",
                ],
            )
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(params.optim_params.early_exag_factor),
            get_f64(
                config,
                &[
                    "lateExagFactor",
                    "late_exag_factor",
                    "lateExaggerationFactor",
                    "late_exaggeration_factor",
                ],
            )
            .filter(|value| value.is_finite() && *value > 0.0),
            get_f64(config, &["theta"])
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or(params.optim_params.theta),
            get_usize(config, &["nInterpPoints", "n_interp_points"]),
        );
    }

    params
}
