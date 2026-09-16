use super::*;

struct FailingCompressor;

impl ImageCompressor for FailingCompressor {
    fn name(&self) -> &str {
        "failing"
    }

    fn compress(
        &self,
        _data: &[u8],
        _media_type: &str,
    ) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        Err("compression failed".into())
    }
}

struct AppendingCompressor;

impl ImageCompressor for AppendingCompressor {
    fn name(&self) -> &str {
        "appending"
    }

    fn compress(
        &self,
        data: &[u8],
        _media_type: &str,
    ) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        let mut output = data.to_vec();
        output.push(9);
        Ok(output)
    }
}

struct RecordingCompressor {
    marker: u8,
    calls: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl ImageCompressor for RecordingCompressor {
    fn name(&self) -> &str {
        "recording"
    }

    fn compress(
        &self,
        data: &[u8],
        _media_type: &str,
    ) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        self.calls.lock().unwrap().push(self.marker);
        let mut output = data.to_vec();
        output.push(self.marker);
        Ok(output)
    }
}

#[test]
fn empty_pipeline_borrows_input() {
    let data = vec![1, 2, 3];
    let pipeline = CompressorPipeline::new();

    let output = pipeline.run(&data, "image/png");
    assert!(matches!(&output, Cow::Borrowed(_)));
    assert_eq!(output.as_ref(), data.as_slice());
}

#[test]
fn failed_first_compressor_borrows_original_input() {
    let data = vec![1, 2, 3];
    let mut pipeline = CompressorPipeline::new();
    pipeline.add(Box::new(FailingCompressor));

    let output = pipeline.run(&data, "image/png");
    assert!(matches!(&output, Cow::Borrowed(_)));
    assert_eq!(output.as_ref(), data.as_slice());
}

#[test]
fn later_compressor_failure_discards_prior_result() {
    let data = vec![1, 2, 3];
    let mut pipeline = CompressorPipeline::new();
    pipeline.add(Box::new(AppendingCompressor));
    pipeline.add(Box::new(FailingCompressor));

    let output = pipeline.run(&data, "image/png");
    assert!(matches!(&output, Cow::Borrowed(_)));
    assert_eq!(output.as_ref(), data.as_slice());
}

#[test]
fn successful_compressors_run_in_order_and_return_each_result() {
    let data = vec![1, 2];
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut pipeline = CompressorPipeline::new();
    pipeline.add(Box::new(RecordingCompressor {
        marker: 3,
        calls: calls.clone(),
    }));
    pipeline.add(Box::new(RecordingCompressor {
        marker: 4,
        calls: calls.clone(),
    }));

    let output = pipeline.run(&data, "image/png");
    assert!(matches!(&output, Cow::Owned(_)));
    assert_eq!(output.as_ref(), &[1, 2, 3, 4]);
    assert_eq!(*calls.lock().unwrap(), vec![3, 4]);
}
