use faer::Mat;
use manifolds_rs::prelude::{NearestNeighbourParams, TsneOptimParams};
use manifolds_rs::TsneParams;
use serde_json::Value;

use crate::umap::{get_bool, get_f64, get_string, get_usize, parse_matrix};

pub(crate) fn tsne_value(data: Option<&Value>, config: Option<&Value>) -> Value {
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

    let data = Mat::from_fn(rows.len(), n_features, |i, j| rows[i][j]);
    let config = config.and_then(Value::as_object);
    let params = params_from_config(config);
    let approx_type = config
        .and_then(|config| get_string(config, &["approxType", "approx_type", "approximation"]))
        .unwrap_or_else(|| "bh".to_string());
    let seed = config
        .and_then(|config| get_usize(config, &["seed"]))
        .unwrap_or(42);
    let verbose = config
        .and_then(|config| get_usize(config, &["verbose"]))
        .unwrap_or(0);

    let approx_type = normalise_approx_type(&approx_type);
    let Some(approx_type) = approx_type.as_deref() else {
        return Value::Null;
    };

    let Ok(embedding) =
        manifolds_rs::tsne(data.as_ref(), None, &params, approx_type, seed, verbose)
    else {
        return Value::Null;
    };

    if embedding.len() != params.n_dim || embedding.iter().any(|dim| dim.len() != rows.len()) {
        return Value::Null;
    }

    Value::Array(
        (0..rows.len())
            .map(|row| {
                Value::Array(
                    (0..params.n_dim)
                        .map(|dim| serde_json::json!(embedding[dim][row]))
                        .collect(),
                )
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

fn normalise_approx_type(approx_type: &str) -> Option<String> {
    match approx_type.to_ascii_lowercase().as_str() {
        "bh" | "barnes hut" | "barnes_hut" | "barnes-hut" | "barneshut" => Some("bh".to_string()),
        // This workspace does not enable manifolds-rs' fft_tsne feature. Passing
        // fft would panic inside manifolds-rs, so treat it as an unsupported config.
        "fft" => None,
        _ => None,
    }
}

