use std::{future::Future, time::Duration};

use tokio::time::sleep;
use tracing::{error, info};

pub(crate) mod once;

#[cfg(not(test))]
fn duration(t: u64) -> Duration {
    Duration::from_secs(t)
}
#[cfg(test)]
fn duration(t: u64) -> Duration {
    Duration::from_millis(t * 100)
}

pub async fn retry<T, E, F, U>(retries: usize, mut f: U) -> Result<T, E>
where
    E: std::fmt::Display,
    F: Future<Output = Result<T, E>>,
    U: FnMut() -> F,
{
    let mut last_err = None;
    for i in 0..=retries {
        if last_err.is_some() {
            let secs = 2u64.saturating_pow(u32::try_from(i).unwrap_or(u32::MAX));
            info!("Retry after {secs} seconds");
            sleep(duration(secs)).await;
        }
        match f().await {
            res @ Ok(_) => return res,
            Err(err) => {
                error!("{err:#}");
                last_err = Some(err);
            }
        }
    }
    Err(last_err.expect("last retry reached after error"))
}

#[cfg(test)]
mod test {
    use std::future::ready;

    use super::*;

    const OK: Result<(), &'static str> = Ok(());
    const ERR: Result<(), &'static str> = Err("err");
    const EPSILON: u64 = 1;

    #[tokio::test]
    async fn zero_retries_success() {
        let earlier = std::time::Instant::now();
        retry(0, || ready(OK)).await.unwrap();
        let t = std::time::Instant::now().duration_since(earlier);
        assert!(t < duration(EPSILON));
    }

    #[tokio::test]
    async fn zero_retries_fail() {
        let earlier = std::time::Instant::now();
        retry(0, || ready(ERR)).await.unwrap_err();
        let t = std::time::Instant::now().duration_since(earlier);
        assert!(t < duration(EPSILON));
    }

    #[tokio::test]
    async fn three_retries_early_success() {
        let mut failures = 2usize;
        let make_fut = || {
            if failures == 0 {
                ready(OK)
            } else {
                failures -= 1;
                ready(ERR)
            }
        };
        let earlier = std::time::Instant::now();
        retry(3, make_fut).await.unwrap();
        let t = std::time::Instant::now().duration_since(earlier);
        assert!(t >= duration(2 + 4));
        assert!(t < duration(2 + 4 + EPSILON));
    }

    #[tokio::test]
    async fn three_retries_fail() {
        let earlier = std::time::Instant::now();
        retry(3, || ready(ERR)).await.unwrap_err();
        let t = std::time::Instant::now().duration_since(earlier);
        assert!(t >= duration(2 + 4 + 8));
        assert!(t < duration(2 + 4 + 8 + EPSILON));
    }
}
