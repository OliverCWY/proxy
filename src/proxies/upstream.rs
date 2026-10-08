trait OutboundCo {
    type Error: std::error::Error;
    async fn proxy(&self) -> Result<(), Self::Error>;
}

