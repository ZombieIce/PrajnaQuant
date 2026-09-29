//! Arrow and Parquet data layer for Prajna Quant.

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles_with_arrow_and_parquet_dependencies() {
        let _schema = arrow_schema::Schema::empty();
        let _batch = arrow_array::RecordBatch::new_empty(std::sync::Arc::new(_schema));
        let _writer_properties = parquet::file::properties::WriterProperties::builder().build();
        let _domain_type = std::any::type_name::<prajna_domain::Price>();
    }
}
