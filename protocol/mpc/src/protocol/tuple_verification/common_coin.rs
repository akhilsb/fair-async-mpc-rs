use lambdaworks_math::{polynomial::Polynomial, traits::ByteConversion};
use protocol::{LargeFieldSer, LargeField};

use crate::{Context, msg::ProtMsg, protocol::tuple_verification::ex_compr_state::ExComprState};

impl Context{
    pub async fn toss_common_coin(&mut self, depth: usize){
        if self.rand_sharings_state.rand_sharings_coin.is_empty() {
            log::warn!("toss_common_coin: No coins left to toss at depth {}. Cannot proceed.", depth);
            return;
        }
        let coin_share = self.rand_sharings_state.rand_sharings_coin.pop_front().unwrap();
        let prot_msg = ProtMsg::ReconstructCoin(coin_share.to_bytes_be(), depth);

        self.broadcast(prot_msg).await;
        if depth == self.delinearization_depth{
            self.verify_coin_toss_deserialization().await;
        }
        else {
            self.verify_level_termination(depth).await;
        }
    }

    pub async fn handle_common_coin_msg(&mut self, lf_share: LargeFieldSer, sender: usize, depth: usize){
        if !self.verf_state.ex_compr_state.contains_key(&depth){
            self.verf_state.ex_compr_state.insert(depth, ExComprState::new(depth));
        }
        let ex_compr_state = self.verf_state.ex_compr_state.get_mut(&depth).unwrap();
        
        let evaluation_point = Self::get_share_evaluation_point(sender, self.use_fft, self.roots_of_unity.clone());
        ex_compr_state.coin_toss_shares.0.push(evaluation_point);
        ex_compr_state.coin_toss_shares.1.push(LargeField::from_bytes_be(&lf_share).unwrap());
        
        log::info!("Received coin toss from sender {} at depth {}", 
            sender, depth);
        
        if ex_compr_state.coin_toss_shares.0.len() >= self.num_faults + 1 && ex_compr_state.coin_output.is_none(){
            // Reconstruct coin with these points
            let polynomial = Polynomial::interpolate(
                &ex_compr_state.coin_toss_shares.0[0..self.num_faults+1], 
                &ex_compr_state.coin_toss_shares.1[0..self.num_faults+1],
            ).unwrap();
            let coin_value = polynomial.evaluate(&LargeField::zero());
            ex_compr_state.coin_output = Some(coin_value.clone());
            if depth == self.delinearization_depth{
                log::info!("Reconstructed common coin at delinearization depth {}: {:?}", depth, ex_compr_state.coin_output);
                self.verify_coin_toss_deserialization().await;
            }
            else{
                // Trigger subsequent phase here. 
                log::info!("Reconstructed common coin at depth {}: {:?}", depth, ex_compr_state.coin_output);
                self.verify_level_termination(depth).await;
            }
        }
    }
}