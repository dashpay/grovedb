/*
use rs_merkle::{algorithms::Sha256, MerkleProof};
use crate::{Element, Error, GroveDb,PathQuery, Proof, Query};
*/

/*
//no need for ProofofMembership struct

struct ProofNonMembership {
    target: Element,
    L: Vec<Element>,
    R: Vec<Element> //optional
};

struct ProofBoundedRange {
    L1: Vec<Element>,//optional
    R1: Vec<Element>,
    L2: Vec<Element>,
    R2: Vec<Element> //optional
};

struct ProofUnboundedRange {
    L: Vec<Element>,
    R: Vec<Element>, //optional 
    E: Vec<Element>
};

impl GroveDb {

    pub fn GenProof(Element: target) -> Vec<Element>{
        //check if the element actually exists first                                    
    }
    
    /*
     * 
     */
    pub fn GenProofNonMembership(Element: target, ) -> ProofNonMembership {
        //check it doesnt exist, pretend to insert it and return proofs of appropriate elements
        //suppose they are L,R
        let (a,b) = extremal(target);
        ProofNonMembership {
            target: target,
            L: GenProof(L),
            R: if a {None} else {GenProof(R)};
        }
        return temp;
    }

    pub fn GenProofBoundedRange(Element: L, Element: R) -> ProofBoundedRange {
        let (a,b) = extremal(L);
        let (c,d) = extremal(R);
        //seek for L-1,R+1
        ProofBoundedRange {
            L1: if a {None} else {GenProof(L-1)},
            R1: GenProof(L),
            L2: GenProof(R),
            R2: if c {None} else {GenProof(R+1)}
        }
    }

    pub fn GenProofUnboundedRange(Element: target, bool:direction) -> ProofUnboundedRange {
        //seek for extremal element, left if direction is 0, right if its 1
        //suppose its called extremal
        let (a,b) = extremal(target)
        //seek for pair element, target2
        ProofUnboundedRange {
            L: GenProof(target),
            R: if a {None} else {GenProof(target2)},
            E: GenProof(Extremal)
        };
    }

    //verify functions


    /*
     * classic, atomic primitive
     */
    pub fn Verify(proof: Vec<Element>) -> bool {

    } 


    /*
     *Both elements must verify correctly, must be adjacent, and target must be between them OR
     *Proof is a single extremal item, and the target is beyond it appropriately
     */
    pub fn VerifyNonMembership(proof: ProofNonMembership, Element: root) -> bool {
        if(!proof.edge) { //normal, not extremal
            return adjacent(proof.L,proof.R) & Verify(proof.L) & Verify(proof.R);
        }
        else {
            let (a,b) = extremal(proof.L);   //a always true if its extremal, b is which way
            if(proof.R == None) {
                return Verify(proof.L) //& proof.target > proof.L element
            }
            else {
                return Verify(proof.L) //& proof.target < proof.L  element
            }
        }
    } 
        
    /*
     * Four elements: all verify, and the two pairs must be adjacent
     * Three elements:all verify, and one pair is adjacent, the other is extremal
     * Two elements:  all verify, all extremal
     */
    pub fn VerifyBoundedRange(proof: BoundedRangeProof, Element: root) -> bool {
        let mut ans = true;
        ans &= Verify(proof.R1);
        ans &= Verify(proof.L2);
        if (proof.L1 != None) {
            ans &= adjacent(proof.L1,proof.R1);
            ans &= Verify(proof.L1);
        }
        else {
            let (a,b) = extremal(proof.R1);
            ans &= a;
            ans &= !b;
        }
        if (proof.R2 != None) {
            ans &= adjacent(proof.L2,proof.R2);
            ans &= Verify(proof.R2);
        }
        else {
            let (a,b) = extremal(proof.L2);
            ans &= a;
            ans &= b;
        }
        return ans;

    }

    /*
     * Three elements: all verify, one pair adjacent, other extremal
     *
     *
     */
    pub fn VerifyUnboundedRange(proof: UnboundedRangeProof, Element: root) -> bool {
        let mut ans = true;
        ans &= Verify(proof.L);
        let (a,b) = extremal(proof.E);
        ans &= a;
        if (proof.R != None){
            ans &= adjacent(proof.L, proof.R);
        }
        if (b) { //limit is right
            //proof.L is left of E
        }
        else {  //limit is left
            //proof.L is right of E
        }
        return ans
    }


    //helper functions

    /*
     *This checks if the element is on an outer most path in the tree, there is no smaller/larger element
     */
    fn extremal(target: Element) -> (bool,bool) { 
    
    } }
    /*
     *Given two elements, are they adjacent? 
     Being adjacent now doesnt mean they will be forever in the future
     */
    fn adjacent(L: Element, R: Element) -> bool {

    }
}

*/
