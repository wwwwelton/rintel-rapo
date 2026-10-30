// Avatarnoia Flow Engine 2.0: online paired-reward predictor, never an actuator.
const INPUTS: usize = 9;
const HIDDEN: usize = 16;

pub struct Flow {
    weights: [[f64; INPUTS]; HIDDEN],
    biases: [f64; HIDDEN],
    output: [f64; HIDDEN],
    bias: f64,
    pub samples: usize,
}
impl Flow {
    pub fn new(mut seed: u64) -> Self {
        seed = seed.max(1);
        let mut weights = [[0.0; INPUTS]; HIDDEN];
        for row in &mut weights {
            for weight in row {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                *weight = (seed as f64 / u64::MAX as f64 - 0.5) * 0.5;
            }
        }
        Self { weights, biases: [0.0; HIDDEN], output: [0.0; HIDDEN], bias: 0.0, samples: 0 }
    }
    fn forward(&self, input: &[f64; INPUTS]) -> ([f64; HIDDEN], f64) {
        let mut hidden = [0.0; HIDDEN];
        let mut prediction = self.bias;
        for (i, value) in hidden.iter_mut().enumerate() {
            let mut sum = self.biases[i];
            for (weight, input) in self.weights[i].iter().zip(input) { sum += weight * input; }
            // Softsign avoids exponential functions and has a bounded derivative.
            *value = sum / (1.0 + sum.abs());
            prediction += self.output[i] * *value;
        }
        (hidden, prediction)
    }
    pub fn predict(&self, input: &[f64; INPUTS]) -> f64 { self.forward(input).1 }
    pub fn train(&mut self, input: &[f64; INPUTS], target: f64) {
        if !target.is_finite() || input.iter().any(|x| !x.is_finite()) { return; }
        let (hidden, prediction) = self.forward(input);
        let error = (prediction - target.clamp(-1.0, 1.0)).clamp(-1.0, 1.0);
        let rate = 0.03;
        for (i, value) in hidden.iter().enumerate() {
            let gradient = error * self.output[i] * (1.0 - value.abs()).powi(2);
            for (weight, input) in self.weights[i].iter_mut().zip(input) {
                *weight -= rate * (gradient * input + 0.0001 * *weight);
            }
            self.biases[i] -= rate * gradient;
            self.output[i] -= rate * (error * value + 0.0001 * self.output[i]);
        }
        self.bias -= rate * error;
        self.samples = self.samples.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn learns_joint_effect_and_adapts_to_reversal() {
        let mut model = Flow::new(42);
        for _ in 0..4000 {
            for (a, b) in [(0.0, 0.0), (0.0, 1.0), (1.0, 0.0), (1.0, 1.0)] {
                model.train(&[a, b, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], if a != b { 0.2 } else { -0.2 });
            }
        }
        assert!(model.predict(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]) > 0.1);
        let input = [1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        for _ in 0..500 { model.train(&input, 0.3); }
        assert!(model.predict(&input) > 0.2);
        let before = model.predict(&input);
        model.train(&input, f64::NAN);
        assert_eq!(model.predict(&input), before);
    }
}
