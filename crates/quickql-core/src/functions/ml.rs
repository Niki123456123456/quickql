use quickql_macros::fn_info;
use serde_json::Value;

use crate::{optics as optics_function, tsne as tsne_function, umap as umap_function};
use crate::{FnInfo, JsonTypeInfo, ParamInfo};

pub(crate) fn infos() -> Vec<FnInfo> {
    vec![
        softmax_info(),
        entropy_info(),
        l2_info(),
        optics_info(),
        tsne_info(),
        umap_info(),
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
fn optics(input: Option<&Value>, options: Option<&Value>) -> Value {
    optics_function::optics_value(input, options)
}

#[fn_info()]
fn tsne(input: Option<&Value>, options: Option<&Value>) -> Value {
    tsne_function::tsne_value(input, options)
}

#[fn_info()]
fn umap(input: Option<&Value>, options: Option<&Value>) -> Value {
    umap_function::umap_value(input, options)
}
